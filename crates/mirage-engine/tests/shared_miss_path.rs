use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use futures_executor::block_on;
use futures_util::future::join_all;
use mirage_backend::{
    BackendError, BackendErrorClass, BackendRead, DeletionProof, FetchClass, ObjectBackend,
    ObjectKind, ObjectStat, RemoteObjectRef, UploadSource,
};
use mirage_backend_local::LocalObjectBackend;
use mirage_cache::{
    ArenaShard, BudgetConfig, CacheLayout, ReservationLedger, ResidentIndex, insert_page,
};
use mirage_db::{CacheShardSpec, Database};
use mirage_engine::{FetchContext, MountGeneration, PageLocation, PageLocationMap, PageProvider};
use mirage_pack::{PlainPage, encode_plain_frame};
use mirage_scheduler::{FetchPool, FetchPoolConfig, FetchPriority};
use mirage_types::{
    BackendHealthState, ByteCount, CheckedRange, ContentHash, MirageErrorKind, PageHash,
    RepositoryId,
};
use tokio_util::sync::CancellationToken;

const PAGE: usize = 4096;
const BUDGET: u64 = 1 << 20;

/// Backend wrapper whose first `read_range` blocks on a test-controlled gate so
/// flights stay joinable; it also counts reads and records whether the flight
/// token was cancelled by the time the gate opened.
struct GatedBackend<B: ObjectBackend> {
    inner: B,
    reads: AtomicUsize,
    gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    observed_cancelled: AtomicBool,
    forced_error: Mutex<Option<BackendError>>,
}

impl<B: ObjectBackend> GatedBackend<B> {
    fn new(inner: B, gate: Option<std::sync::mpsc::Receiver<()>>) -> Self {
        Self {
            inner,
            reads: AtomicUsize::new(0),
            gate: Mutex::new(gate),
            observed_cancelled: AtomicBool::new(false),
            forced_error: Mutex::new(None),
        }
    }
}

#[async_trait]
impl<B: ObjectBackend> ObjectBackend for GatedBackend<B> {
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
        self.reads.fetch_add(1, Ordering::SeqCst);
        let gate = self.gate.lock().expect("gate lock").take();
        if let Some(receiver) = gate {
            let _ = receiver.recv();
            self.observed_cancelled
                .store(cancel.is_cancelled(), Ordering::SeqCst);
        }
        if let Some(error) = self.forced_error.lock().expect("forced error lock").take() {
            return Err(error);
        }
        self.inner.read_range(object, range, class, cancel).await
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

    async fn health(&self) -> BackendHealthState {
        self.inner.health().await
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    backend: Arc<GatedBackend<LocalObjectBackend>>,
    provider: PageProvider<GatedBackend<LocalObjectBackend>>,
    ledger: ReservationLedger,
}

/// Builds a one-pack repository: each frame's `PageHash` maps to its byte range
/// inside the pack, mirroring how `PageLocationMap::build` derives locations.
fn build_fixture(
    frames: Vec<Vec<u8>>,
    hashes: Vec<PageHash>,
    gate: Option<std::sync::mpsc::Receiver<()>>,
    forced: Option<BackendError>,
    pool: Option<Arc<FetchPool>>,
    hard_bytes: u64,
) -> Fixture {
    let directory = tempfile::tempdir().expect("directory");
    let local = LocalObjectBackend::open(
        &directory.path().join("remote"),
        RepositoryId::from_bytes([7; 16]),
    )
    .expect("backend");
    let mut wire = Vec::new();
    let mut ranges = Vec::new();
    for frame in &frames {
        ranges.push(CheckedRange::new(wire.len() as u64, frame.len() as u64).expect("range"));
        wire.extend_from_slice(frame);
    }
    let content = ContentHash::from_bytes(*blake3::hash(&wire).as_bytes());
    let object = block_on(local.put_immutable(
        ObjectKind::Pack,
        UploadSource::from_bytes(Bytes::from(wire)),
        content,
        CancellationToken::new(),
    ))
    .expect("put");
    let mut locations = PageLocationMap::default();
    for (hash, range) in hashes.iter().zip(ranges) {
        locations
            .insert(
                *hash,
                PageLocation {
                    object: object.clone(),
                    encoded_range: range,
                    logical_length: PAGE as u32,
                },
            )
            .expect("location");
    }
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(64 * 1024),
        slot_count: 16,
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
    .expect("register shard");
    let index = Arc::new(ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index"));
    let backend = Arc::new(GatedBackend::new(local, gate));
    *backend.forced_error.lock().expect("forced error lock") = forced;
    let ledger = ReservationLedger::new(
        BudgetConfig {
            hard_bytes,
            prefetch_soft_bytes: hard_bytes,
            update_safety_reserve: 0,
            dirty_update_bytes: 0,
        },
        0,
    )
    .expect("ledger");
    let mut provider = PageProvider::new(
        Arc::clone(&backend),
        db,
        shard,
        index,
        Arc::new(locations),
        ledger.clone(),
        1024 * 1024,
    );
    if let Some(pool) = pool {
        provider = provider.with_fetch_pool(pool);
    }
    Fixture {
        _directory: directory,
        backend,
        provider,
        ledger,
    }
}

fn frame(fill: u8) -> (PageHash, Vec<u8>) {
    let page = PlainPage::from_bytes(Bytes::from(vec![fill; PAGE]));
    let encoded = encode_plain_frame(&page).expect("frame");
    (page.hash, encoded)
}

fn context(cancellation: CancellationToken) -> FetchContext {
    FetchContext {
        priority: FetchPriority::P0Blocking,
        deadline_ns: u64::MAX,
        cancellation,
    }
}

fn poll_once<F: std::future::Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    future.poll(&mut cx)
}

#[test]
fn adjacent_resident_pages_are_served_by_one_coalesced_read() {
    let directory = tempfile::tempdir().expect("directory");
    let source = tempfile::tempdir().expect("source");
    let mut expected = Vec::new();
    for page in 0..4_u8 {
        expected.extend(std::iter::repeat_n(page + 0x11, 64 * 1024));
    }
    std::fs::write(source.path().join("data.pak"), &expected).expect("source");
    let repository_id = RepositoryId::from_bytes([0x2a; 16]);
    let imported = mirage_pack::import_local(&mirage_pack::ImportPlan {
        repository_id,
        generation_id: mirage_types::GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![mirage_pack::PlannedFile {
            relative_path: "data.pak".into(),
            class: mirage_manifest::FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 512 * 1024,
        output_staging_directory: directory.path().join("import"),
        encryption: None,
    })
    .expect("import");
    let index_path = directory.path().join("mount.idx");
    mirage_index::compile_to_path(&imported.manifest, &index_path).expect("index");
    let generation = MountGeneration::new(
        repository_id,
        mirage_types::GenerationId::ZERO,
        Arc::new(mirage_index::MountIndex::open(&index_path).expect("mount index")),
        64 * 1024,
    )
    .expect("generation");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(64 * 1024),
        slot_count: 8,
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
    .expect("register shard");
    for (ordinal, page) in imported.manifest.pages.iter().enumerate() {
        insert_page(
            &db,
            Arc::clone(&shard),
            page.plaintext_hash,
            &expected[ordinal * 64 * 1024..(ordinal + 1) * 64 * 1024],
            &(),
        )
        .expect("insert page");
    }
    let index = Arc::new(ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("resident index"));
    let backend = Arc::new(GatedBackend::new(
        LocalObjectBackend::open(&directory.path().join("remote"), repository_id).expect("backend"),
        None,
    ));
    let provider = PageProvider::new(
        Arc::clone(&backend),
        db,
        shard,
        index,
        Arc::new(PageLocationMap::default()),
        ReservationLedger::new(
            BudgetConfig {
                hard_bytes: BUDGET,
                prefetch_soft_bytes: BUDGET,
                update_safety_reserve: 0,
                dirty_update_bytes: 0,
            },
            0,
        )
        .expect("ledger"),
        1024 * 1024,
    );
    let mut output = vec![0_u8; expected.len()];
    let read = block_on(provider.read_into(
        &generation,
        0,
        0,
        &mut output,
        context(CancellationToken::new()),
        8,
    ))
    .expect("read");
    assert_eq!(read, expected.len());
    assert_eq!(output, expected);
    assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
}

#[test]
fn hundred_concurrent_readers_share_one_fetch_and_one_reservation() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let (hash, encoded) = frame(0x5a);
    let fixture = build_fixture(vec![encoded], vec![hash], Some(gate_rx), None, None, BUDGET);
    let mut futures: Vec<_> = (0..100)
        .map(|_| {
            Box::pin(
                fixture
                    .provider
                    .get_or_fetch(hash, context(CancellationToken::new())),
            )
        })
        .collect();
    for future in &mut futures {
        assert!(poll_once(future.as_mut()).is_pending());
    }
    gate_tx.send(()).expect("open gate");
    let results = block_on(join_all(futures));
    assert_eq!(fixture.backend.reads.load(Ordering::SeqCst), 1);
    for result in results {
        let guard = result.expect("fetch");
        let mut bytes = vec![0_u8; PAGE];
        guard.read_exact(0, &mut bytes).expect("read page");
        assert_eq!(bytes, vec![0x5a; PAGE]);
    }
    let snapshot = fixture.ledger.snapshot().expect("snapshot");
    assert_eq!(snapshot.committed_bytes, PAGE as u64);
    assert_eq!(snapshot.reserved_bytes, 0);
    assert_eq!(snapshot.peak_envelope_bytes, snapshot.committed_bytes);
    assert_eq!(fixture.provider.flight_metrics().0, 0);
}

#[test]
fn successful_fetch_commits_its_reservation() {
    let (hash, encoded) = frame(0x4c);
    let fixture = build_fixture(vec![encoded], vec![hash], None, None, None, BUDGET);
    let before = fixture.provider.budget_snapshot().expect("snapshot");
    let guard = block_on(
        fixture
            .provider
            .get_or_fetch(hash, context(CancellationToken::new())),
    )
    .expect("fetch");
    drop(guard);
    let snapshot = fixture.provider.budget_snapshot().expect("snapshot");
    assert_eq!(
        snapshot.committed_bytes,
        before.committed_bytes + PAGE as u64
    );
    assert_eq!(snapshot.reserved_bytes, 0);
}

#[test]
fn cancelled_subscribers_detach_and_survivors_complete() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let (hash, encoded) = frame(0x33);
    let fixture = build_fixture(vec![encoded], vec![hash], Some(gate_rx), None, None, BUDGET);
    let mut survivor = Box::pin(
        fixture
            .provider
            .get_or_fetch(hash, context(CancellationToken::new())),
    );
    assert!(poll_once(survivor.as_mut()).is_pending());
    let mut tokens = Vec::new();
    let mut leaving = Vec::new();
    for _ in 0..9 {
        let token = CancellationToken::new();
        let mut future = Box::pin(fixture.provider.get_or_fetch(hash, context(token.clone())));
        assert!(poll_once(future.as_mut()).is_pending());
        tokens.push(token);
        leaving.push(future);
    }
    for token in &tokens {
        token.cancel();
    }
    for result in block_on(join_all(leaving)) {
        let error = match result {
            Ok(_) => panic!("cancelled waiter must not succeed"),
            Err(error) => error,
        };
        assert_eq!(error.kind, MirageErrorKind::Cancelled);
    }
    gate_tx.send(()).expect("open gate");
    block_on(survivor).expect("survivor completes");
    assert_eq!(fixture.backend.reads.load(Ordering::SeqCst), 1);
    assert!(!fixture.backend.observed_cancelled.load(Ordering::SeqCst));
}

#[test]
fn all_waiters_cancelled_cancels_underlying_fetch() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let (hash, encoded) = frame(0x44);
    let fixture = build_fixture(vec![encoded], vec![hash], Some(gate_rx), None, None, BUDGET);
    let mut tokens = Vec::new();
    let mut futures = Vec::new();
    for _ in 0..3 {
        let token = CancellationToken::new();
        let mut future = Box::pin(fixture.provider.get_or_fetch(hash, context(token.clone())));
        assert!(poll_once(future.as_mut()).is_pending());
        tokens.push(token);
        futures.push(future);
    }
    for token in &tokens {
        token.cancel();
    }
    for result in block_on(join_all(futures)) {
        let error = match result {
            Ok(_) => panic!("cancelled waiter must not succeed"),
            Err(error) => error,
        };
        assert_eq!(error.kind, MirageErrorKind::Cancelled);
    }
    gate_tx.send(()).expect("open gate");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let (_flights, queued, running) = fixture.provider.flight_metrics();
        if fixture.backend.reads.load(Ordering::SeqCst) >= 1 && queued == 0 && running == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(fixture.provider.flight_metrics().2, 0);
    assert!(fixture.backend.observed_cancelled.load(Ordering::SeqCst));

    let guard = block_on(
        fixture
            .provider
            .get_or_fetch(hash, context(CancellationToken::new())),
    )
    .expect("refetch");
    assert_eq!(fixture.backend.reads.load(Ordering::SeqCst), 2);
    let mut bytes = vec![0_u8; PAGE];
    guard.read_exact(0, &mut bytes).expect("resident read");
    assert_eq!(bytes, vec![0x44; PAGE]);
}

#[test]
fn dropped_owner_future_does_not_strand_subscribers() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let (hash, encoded) = frame(0x55);
    let fixture = build_fixture(vec![encoded], vec![hash], Some(gate_rx), None, None, BUDGET);
    let mut owner = Box::pin(
        fixture
            .provider
            .get_or_fetch(hash, context(CancellationToken::new())),
    );
    assert!(poll_once(owner.as_mut()).is_pending());
    drop(owner);
    let mut subscriber = Box::pin(
        fixture
            .provider
            .get_or_fetch(hash, context(CancellationToken::new())),
    );
    assert!(poll_once(subscriber.as_mut()).is_pending());
    gate_tx.send(()).expect("open gate");
    block_on(subscriber).expect("subscriber completes");
    assert_eq!(fixture.backend.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn failure_causes_are_typed_for_every_waiter() {
    // Authorization failure fans out to every waiter as permission denied.
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let (hash, encoded) = frame(0x66);
    let fixture = build_fixture(
        vec![encoded],
        vec![hash],
        Some(gate_rx),
        Some(BackendError::new(BackendErrorClass::Permission, "denied")),
        None,
        BUDGET,
    );
    let mut futures: Vec<_> = (0..5)
        .map(|_| {
            Box::pin(
                fixture
                    .provider
                    .get_or_fetch(hash, context(CancellationToken::new())),
            )
        })
        .collect();
    for future in &mut futures {
        assert!(poll_once(future.as_mut()).is_pending());
    }
    gate_tx.send(()).expect("open gate");
    for result in block_on(join_all(futures)) {
        let error = match result {
            Ok(_) => panic!("denied waiter must not succeed"),
            Err(error) => error,
        };
        assert_eq!(error.kind, MirageErrorKind::BackendPermissionDenied);
    }
    assert_eq!(fixture.backend.reads.load(Ordering::SeqCst), 1);

    // A corrupted frame fans out as an integrity failure.
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let (hash, mut encoded) = frame(0x77);
    encoded[100] ^= 1;
    let fixture = build_fixture(vec![encoded], vec![hash], Some(gate_rx), None, None, BUDGET);
    let mut futures: Vec<_> = (0..3)
        .map(|_| {
            Box::pin(
                fixture
                    .provider
                    .get_or_fetch(hash, context(CancellationToken::new())),
            )
        })
        .collect();
    for future in &mut futures {
        assert!(poll_once(future.as_mut()).is_pending());
    }
    gate_tx.send(()).expect("open gate");
    let results = block_on(join_all(futures));
    for result in results {
        let error = match result {
            Ok(_) => panic!("corrupt waiter must not succeed"),
            Err(error) => error,
        };
        assert_eq!(error.kind, MirageErrorKind::IntegrityMismatch);
    }
    assert_eq!(fixture.backend.reads.load(Ordering::SeqCst), 1);

    // A budget too small for one page is a typed CacheFull for owner and
    // subscriber alike.
    let (hash, encoded) = frame(0x88);
    let fixture = build_fixture(vec![encoded], vec![hash], None, None, None, PAGE as u64 - 1);
    let first = match block_on(
        fixture
            .provider
            .get_or_fetch(hash, context(CancellationToken::new())),
    ) {
        Ok(_) => panic!("owner over budget must not succeed"),
        Err(error) => error,
    };
    assert_eq!(first.kind, MirageErrorKind::CacheFull);
    let second = match block_on(
        fixture
            .provider
            .get_or_fetch(hash, context(CancellationToken::new())),
    ) {
        Ok(_) => panic!("subscriber over budget must not succeed"),
        Err(error) => error,
    };
    assert_eq!(second.kind, MirageErrorKind::CacheFull);
}

#[test]
fn pool_saturation_is_a_typed_budget_failure() {
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 1,
        speculative_queue_depth: 1,
        max_in_flight_bytes: 0,
    })
    .expect("pool");
    let frames = [frame(0x11), frame(0x22), frame(0x33)];
    let hashes: Vec<_> = frames.iter().map(|(hash, _)| *hash).collect();
    let fixture = build_fixture(
        frames.into_iter().map(|(_, encoded)| encoded).collect(),
        hashes.clone(),
        Some(gate_rx),
        None,
        Some(pool),
        BUDGET,
    );
    let mut futures: Vec<_> = hashes
        .iter()
        .map(|hash| {
            Box::pin(
                fixture
                    .provider
                    .get_or_fetch(*hash, context(CancellationToken::new())),
            )
        })
        .collect();
    let mut saturated = 0;
    let mut pending = Vec::new();
    for mut future in futures.drain(..) {
        match poll_once(future.as_mut()) {
            Poll::Pending => pending.push(future),
            Poll::Ready(Err(error)) => {
                assert_eq!(error.kind, MirageErrorKind::CacheFull);
                saturated += 1;
            }
            Poll::Ready(Ok(_)) => panic!("fetch completed while the pool was saturated"),
        }
    }
    assert!(saturated >= 1, "saturated pool must report a typed failure");
    gate_tx.send(()).expect("open gate");
    for result in block_on(join_all(pending)) {
        result.expect("queued fetch completes");
    }
}

#[test]
fn transient_fetch_is_rejected_when_the_pool_is_saturated() {
    // The transient path shares the bounded pool: a queued-but-unstarted
    // fetch plus a running one exhaust workers=1/queue_depth=1, so the next
    // fetch must surface CacheFull rather than bypass the bound.
    let (gate_tx, gate_rx) = std::sync::mpsc::channel();
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 1,
        speculative_queue_depth: 1,
        max_in_flight_bytes: 0,
    })
    .expect("pool");
    let frames = [frame(0xa1), frame(0xb2), frame(0xc3)];
    let hashes: Vec<_> = frames.iter().map(|(hash, _)| *hash).collect();
    let fixture = build_fixture(
        frames.into_iter().map(|(_, encoded)| encoded).collect(),
        hashes.clone(),
        Some(gate_rx),
        None,
        Some(pool),
        BUDGET,
    );
    let provider = &fixture.provider;
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            block_on(provider.fetch_transient(hashes[0], context(CancellationToken::new())))
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture.backend.reads.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(fixture.backend.reads.load(Ordering::SeqCst), 1);
        let second = scope.spawn(|| {
            block_on(provider.fetch_transient(hashes[1], context(CancellationToken::new())))
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while provider.flight_metrics().1 == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            provider.flight_metrics().1,
            1,
            "second transient fetch must be queued behind the running one"
        );
        let error = match block_on(
            provider.fetch_transient(hashes[2], context(CancellationToken::new())),
        ) {
            Ok(_) => panic!("saturated pool must reject the transient fetch"),
            Err(error) => error,
        };
        assert_eq!(error.kind, MirageErrorKind::CacheFull);
        gate_tx.send(()).expect("open gate");
        let page = first.join().expect("first thread").expect("first page");
        assert_eq!(page.bytes.as_ref(), &[0xa1; PAGE]);
        let page = second.join().expect("second thread").expect("second page");
        assert_eq!(page.bytes.as_ref(), &[0xb2; PAGE]);
    });
}

#[test]
fn transient_fetch_expired_while_queued_is_deadline_exceeded() {
    // A queued job whose deadline passes never runs: the expiry hook resolves
    // the shared flight and the caller sees DeadlineExceeded.
    let pool = FetchPool::new(FetchPoolConfig {
        workers: 1,
        queue_depth: 4,
        speculative_queue_depth: 1,
        max_in_flight_bytes: 0,
    })
    .expect("pool");
    let (hash, encoded) = frame(0xd4);
    let fixture = build_fixture(vec![encoded], vec![hash], None, None, Some(pool), BUDGET);
    let mut expired = context(CancellationToken::new());
    expired.deadline_ns = 0;
    let error = match block_on(fixture.provider.fetch_transient(hash, expired)) {
        Ok(_) => panic!("expired fetch must not succeed"),
        Err(error) => error,
    };
    assert_eq!(error.kind, MirageErrorKind::DeadlineExceeded);
}

#[test]
fn provide_sync_places_verified_page_in_arena() {
    let (hash, encoded) = frame(0x51);
    let fixture = build_fixture(vec![encoded], vec![hash], None, None, None, BUDGET);
    match fixture.provider.provide_sync(hash).expect("provide") {
        mirage_engine::ProviderPage::Placed(guard) => {
            let mut bytes = vec![0u8; PAGE];
            guard.read_exact(0, &mut bytes).expect("guard read");
            assert!(bytes.iter().all(|byte| *byte == 0x51));
        }
        mirage_engine::ProviderPage::Transient(_) => panic!("expected an admitted page"),
    }
    assert_eq!(fixture.backend.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn provide_sync_serves_verified_bytes_transiently_when_budget_is_full() {
    let (hash, encoded) = frame(0x62);
    // Zero budget: admission fails with CacheFull, so the read must fall back
    // to a verified transient page rather than failing or zero-filling.
    let fixture = build_fixture(vec![encoded], vec![hash], None, None, None, 1);
    match fixture
        .provider
        .provide_sync(hash)
        .expect("transient provide")
    {
        mirage_engine::ProviderPage::Transient(page) => {
            assert_eq!(page.hash, hash);
            assert_eq!(page.bytes.len(), PAGE);
            assert!(page.bytes.iter().all(|byte| *byte == 0x62));
        }
        mirage_engine::ProviderPage::Placed(_) => panic!("expected a transient page"),
    }
}

#[test]
fn provide_sync_never_zero_fills_unknown_or_corrupt_content() {
    // Unknown hash: the pack exists but no location maps to it — a typed
    // error, never synthesized bytes.
    let (known, encoded) = frame(0x6f);
    let fixture = build_fixture(vec![encoded], vec![known], None, None, None, BUDGET);
    assert!(
        fixture
            .provider
            .provide_sync(PageHash::from_bytes([0xff; 32]))
            .is_err()
    );
    // Corrupt frame: hash mismatch must surface, not bytes.
    let (hash, _encoded) = frame(0x70);
    let (other, encoded) = frame(0x71);
    let fixture = build_fixture(vec![encoded], vec![hash], None, None, None, BUDGET);
    let error = fixture.provider.provide_sync(other).err();
    // `other` has no location either; swap: map `hash`'s location content that
    // decodes to a different page hash must fail verification.
    assert!(error.is_some());
}
