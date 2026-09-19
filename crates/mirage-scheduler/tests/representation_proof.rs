use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures_executor::block_on;
use futures_util::stream;
use mirage_backend::{
    BackendByteStream, BackendError, BackendRead, DeletionProof, FetchClass, ObjectBackend,
    ObjectKind, ObjectStat, RemoteObjectRef, UploadSource,
};
use mirage_backend_local::LocalObjectBackend;
use mirage_cache::{ArenaShard, CacheLayout, ResidentIndex};
use mirage_crypto::aead::RepositoryKey;
use mirage_db::{CacheShardSpec, Database};
use mirage_pack::{
    EncryptedFrameAad, PackReadEncryption, PlainPage, decode_plain_frame, encode_encrypted_frame,
    encode_plain_frame,
};
use mirage_scheduler::{FetchPriority, FetchWindow, FrameMapping, fetch_window_with_encryption};
use mirage_types::{ByteCount, CheckedRange, ContentHash, MirageErrorKind, PageHash, RepositoryId};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
enum Rewrite {
    /// Declares an inflated stream length but delivers the honest body.
    InflateDeclared(u64),
    /// Declares the honest length but ends the body early.
    TruncateBody,
    /// Declares the honest length but appends an extra byte.
    ExtendBody,
    /// Produces a zero-length chunk before the body.
    EmptyChunk,
}

/// Delegating backend that can rewrite the response stream while keeping the
/// honest bytes — exercises representation-level proof checks in fetch_window.
struct RewritingBackend<'a, B> {
    inner: &'a B,
    rewrite: Rewrite,
}

#[async_trait]
impl<B: ObjectBackend> ObjectBackend for RewritingBackend<'_, B> {
    fn capabilities(&self) -> mirage_backend::BackendCapabilities {
        self.inner.capabilities()
    }

    async fn read_range(
        &self,
        object: &RemoteObjectRef,
        range: CheckedRange,
        class: FetchClass,
        cancel: CancellationToken,
    ) -> Result<BackendRead, BackendError> {
        let read = self.inner.read_range(object, range, class, cancel).await?;
        let Rewrite::InflateDeclared(declared) = self.rewrite else {
            let declared = read.received_length.as_u64();
            let body = read.stream.collect_bounded(declared).await?;
            let (chunks, expected) = match self.rewrite {
                Rewrite::TruncateBody => (vec![Ok(body.slice(..body.len() - 1))], declared),
                Rewrite::ExtendBody => (vec![Ok(body), Ok(Bytes::from_static(&[0u8]))], declared),
                Rewrite::EmptyChunk => (vec![Ok(Bytes::new()), Ok(body)], declared),
                Rewrite::InflateDeclared(_) => unreachable!(),
            };
            let stream = BackendByteStream::new(stream::iter(chunks), expected);
            return Ok(BackendRead { stream, ..read });
        };
        let body = read
            .stream
            .collect_bounded(read.received_length.as_u64())
            .await?;
        Ok(BackendRead {
            stream: BackendByteStream::new(stream::once(async move { Ok(body) }), declared),
            ..read
        })
    }

    async fn put_immutable(
        &self,
        kind: ObjectKind,
        source: UploadSource,
        expected_hash: ContentHash,
        cancel: CancellationToken,
    ) -> Result<RemoteObjectRef, BackendError> {
        self.inner
            .put_immutable(kind, source, expected_hash, cancel)
            .await
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
        object: &RemoteObjectRef,
        proof: &DeletionProof,
        cancel: CancellationToken,
    ) -> Result<(), BackendError> {
        self.inner.delete_immutable(object, proof, cancel).await
    }

    async fn health(&self) -> mirage_types::BackendHealthState {
        self.inner.health().await
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    backend: LocalObjectBackend,
    db: Database,
    shard: Arc<ArenaShard>,
    index: ResidentIndex,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("directory");
    let backend = LocalObjectBackend::open(
        &directory.path().join("remote"),
        RepositoryId::from_bytes([1; 16]),
    )
    .expect("backend");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(64 * 1024),
        slot_count: 4,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let shard =
        Arc::new(ArenaShard::create(&directory.path().join("arena.bin"), layout).expect("arena"));
    let db = Database::open(&directory.path().join("control.db")).expect("db");
    db.register_cache_shard(CacheShardSpec {
        shard_id: 0,
        relative_path: "arena.bin".into(),
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    })
    .expect("register");
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index");
    Fixture {
        _directory: directory,
        backend,
        db,
        shard,
        index,
    }
}

fn put_wire(backend: &LocalObjectBackend, wire: &[u8]) -> RemoteObjectRef {
    let content = ContentHash::from_bytes(*blake3::hash(wire).as_bytes());
    block_on(backend.put_immutable(
        ObjectKind::Pack,
        UploadSource::from_bytes(Bytes::copy_from_slice(wire)),
        content,
        CancellationToken::new(),
    ))
    .expect("put")
}

fn window(object: RemoteObjectRef, frames: Vec<FrameMapping>, length: u64) -> FetchWindow {
    FetchWindow {
        object,
        range: CheckedRange::new(0, length).expect("range"),
        priority: FetchPriority::P0Blocking,
        frames,
        gap_bytes: 0,
    }
}

fn mapping(hash: PageHash, offset: u64, length: u64) -> FrameMapping {
    FrameMapping {
        page_hash: hash,
        window_offset: offset,
        encoded_length: length,
    }
}

fn resident(fixture: &Fixture, hash: PageHash) -> bool {
    fixture.index.acquire(hash).expect("lookup").is_some()
}

#[test]
fn window_larger_than_collection_bound_is_rejected_before_placement() {
    let fixture = fixture();
    let page = PlainPage::from_bytes(Bytes::from(vec![7; 4096]));
    let frame = encode_plain_frame(&page).expect("frame");
    let object = put_wire(&fixture.backend, &frame);
    let request = window(
        object,
        vec![mapping(page.hash, 0, frame.len() as u64)],
        frame.len() as u64,
    );
    let error = block_on(fetch_window_with_encryption(
        &fixture.backend,
        request,
        &fixture.db,
        Arc::clone(&fixture.shard),
        &fixture.index,
        CancellationToken::new(),
        1024,
        None,
    ))
    .expect_err("declared length above maximum_window must fail before placement");
    assert_eq!(error.kind, MirageErrorKind::BackendUnavailable);
    assert!(!resident(&fixture, page.hash));
}

#[test]
fn inflated_declared_length_is_rejected_before_draining() {
    let fixture = fixture();
    let page = PlainPage::from_bytes(Bytes::from(vec![8; 4096]));
    let frame = encode_plain_frame(&page).expect("frame");
    let object = put_wire(&fixture.backend, &frame);
    let backend = RewritingBackend {
        inner: &fixture.backend,
        rewrite: Rewrite::InflateDeclared(1024 * 1024),
    };
    let request = window(
        object,
        vec![mapping(page.hash, 0, frame.len() as u64)],
        frame.len() as u64,
    );
    let error = block_on(fetch_window_with_encryption(
        &backend,
        request,
        &fixture.db,
        Arc::clone(&fixture.shard),
        &fixture.index,
        CancellationToken::new(),
        64 * 1024,
        None,
    ))
    .expect_err("declared stream length above maximum_window must fail");
    assert_eq!(error.kind, MirageErrorKind::BackendUnavailable);
    assert!(!resident(&fixture, page.hash));
}

#[test]
fn truncated_and_overrunning_bodies_are_rejected_and_never_resident() {
    for (rewrite, fill) in [(Rewrite::TruncateBody, 9_u8), (Rewrite::ExtendBody, 10_u8)] {
        let fixture = fixture();
        let page = PlainPage::from_bytes(Bytes::from(vec![fill; 4096]));
        let frame = encode_plain_frame(&page).expect("frame");
        let object = put_wire(&fixture.backend, &frame);
        let backend = RewritingBackend {
            inner: &fixture.backend,
            rewrite,
        };
        let request = window(
            object,
            vec![mapping(page.hash, 0, frame.len() as u64)],
            frame.len() as u64,
        );
        let error = block_on(fetch_window_with_encryption(
            &backend,
            request,
            &fixture.db,
            Arc::clone(&fixture.shard),
            &fixture.index,
            CancellationToken::new(),
            64 * 1024,
            None,
        ))
        .expect_err("body length mismatch must fail");
        assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
        assert!(!resident(&fixture, page.hash));
    }
}

#[test]
fn empty_stream_chunk_is_rejected() {
    let fixture = fixture();
    let page = PlainPage::from_bytes(Bytes::from(vec![11; 4096]));
    let frame = encode_plain_frame(&page).expect("frame");
    let object = put_wire(&fixture.backend, &frame);
    let backend = RewritingBackend {
        inner: &fixture.backend,
        rewrite: Rewrite::EmptyChunk,
    };
    let request = window(
        object,
        vec![mapping(page.hash, 0, frame.len() as u64)],
        frame.len() as u64,
    );
    let error = block_on(fetch_window_with_encryption(
        &backend,
        request,
        &fixture.db,
        Arc::clone(&fixture.shard),
        &fixture.index,
        CancellationToken::new(),
        64 * 1024,
        None,
    ))
    .expect_err("empty stream chunk must fail");
    assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
    assert!(!resident(&fixture, page.hash));
}

#[test]
fn tampered_frame_byte_is_integrity_mismatch_and_never_resident() {
    let fixture = fixture();
    let page = PlainPage::from_bytes(Bytes::from(vec![12; 4096]));
    let mut frame = encode_plain_frame(&page).expect("frame");
    frame[100] ^= 1;
    let object = put_wire(&fixture.backend, &frame);
    let request = window(
        object,
        vec![mapping(page.hash, 0, frame.len() as u64)],
        frame.len() as u64,
    );
    let results = block_on(fetch_window_with_encryption(
        &fixture.backend,
        request,
        &fixture.db,
        Arc::clone(&fixture.shard),
        &fixture.index,
        CancellationToken::new(),
        64 * 1024,
        None,
    ))
    .expect("fetch returns per-frame results");
    let error = results[0].result.as_ref().expect_err("tampered frame");
    assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
    assert!(!resident(&fixture, page.hash));
}

#[test]
fn frame_with_wrong_declared_plaintext_length_is_rejected() {
    let fixture = fixture();
    // A structurally valid frame of a different-sized page is returned where the
    // mapping expects the larger page.
    let small = PlainPage::from_bytes(Bytes::from(vec![13; 2048]));
    let expected = PlainPage::from_bytes(Bytes::from(vec![13; 4096]));
    let frame = encode_plain_frame(&small).expect("frame");
    assert!(decode_plain_frame(&frame).is_ok());
    let object = put_wire(&fixture.backend, &frame);
    let request = window(
        object,
        vec![mapping(expected.hash, 0, frame.len() as u64)],
        frame.len() as u64,
    );
    let results = block_on(fetch_window_with_encryption(
        &fixture.backend,
        request,
        &fixture.db,
        Arc::clone(&fixture.shard),
        &fixture.index,
        CancellationToken::new(),
        64 * 1024,
        None,
    ))
    .expect("fetch returns per-frame results");
    let error = results[0]
        .result
        .as_ref()
        .expect_err("wrong plaintext length");
    assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
    assert!(!resident(&fixture, expected.hash));
}

#[test]
fn frame_mapping_escaping_the_response_body_is_rejected() {
    let fixture = fixture();
    let page = PlainPage::from_bytes(Bytes::from(vec![14; 4096]));
    let frame = encode_plain_frame(&page).expect("frame");
    let object = put_wire(&fixture.backend, &frame);
    let request = window(
        object,
        vec![mapping(page.hash, 0, frame.len() as u64 + 1)],
        frame.len() as u64,
    );
    let results = block_on(fetch_window_with_encryption(
        &fixture.backend,
        request,
        &fixture.db,
        Arc::clone(&fixture.shard),
        &fixture.index,
        CancellationToken::new(),
        64 * 1024,
        None,
    ))
    .expect("fetch returns per-frame results");
    let error = results[0].result.as_ref().expect_err("escaping mapping");
    assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
    assert!(!resident(&fixture, page.hash));
}

#[test]
fn encrypted_frame_with_wrong_key_is_rejected_and_never_resident() {
    let fixture = fixture();
    let repository = RepositoryId::from_bytes([1; 16]);
    let page = PlainPage::from_bytes(Bytes::from(vec![15; 4096]));
    let key = RepositoryKey::from_bytes([0x5a; 32]);
    let frame = encode_encrypted_frame(
        &key,
        &page.bytes,
        EncryptedFrameAad {
            repository,
            pack_id: [9; 16],
            frame_index: 0,
            plaintext_hash: page.hash,
            plaintext_length: 0,
        },
    )
    .expect("encode");
    let object = put_wire(&fixture.backend, &frame);
    let frame_len = frame.len() as u64;
    let request =
        |object: RemoteObjectRef| window(object, vec![mapping(page.hash, 0, frame_len)], frame_len);
    let wrong = PackReadEncryption {
        repository_id: repository,
        key: Arc::new(RepositoryKey::from_bytes([0x6b; 32])),
    };
    let results = block_on(fetch_window_with_encryption(
        &fixture.backend,
        request(object.clone()),
        &fixture.db,
        Arc::clone(&fixture.shard),
        &fixture.index,
        CancellationToken::new(),
        64 * 1024,
        Some(&wrong),
    ))
    .expect("fetch returns per-frame results");
    let error = results[0].result.as_ref().expect_err("wrong key frame");
    assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
    assert!(!resident(&fixture, page.hash));

    let right = PackReadEncryption {
        repository_id: repository,
        key: Arc::new(key),
    };
    let results = block_on(fetch_window_with_encryption(
        &fixture.backend,
        request(object),
        &fixture.db,
        Arc::clone(&fixture.shard),
        &fixture.index,
        CancellationToken::new(),
        64 * 1024,
        Some(&right),
    ))
    .expect("fetch returns per-frame results");
    assert!(results[0].result.is_ok());
    assert!(resident(&fixture, page.hash));
}
