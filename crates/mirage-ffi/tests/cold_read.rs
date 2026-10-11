#![allow(unsafe_code)]

//! Cold-read path for evicted payloads: parallel frame fetches in
//! `RemotePayloadStore::read`, remote-first byte service with a bounded
//! background re-stage, and sequential lookahead across an inode's payload
//! chain — all through the directory-backend seam.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MiragePublicationStats, MirageStatus,
    mirage_engine_create_managed_local_provider, mirage_engine_destroy,
    mirage_engine_evict_published, mirage_engine_mark_mounted, mirage_engine_publication_stats,
    mirage_engine_set_drive_token, mirage_file_close, mirage_flush, mirage_lookup,
    mirage_namespace_create, mirage_read, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const BUDGET: u64 = 64 * 1024 * 1024;

fn build_index() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), vec![0x5Au8; 64 * 1024]).expect("seed");
    let objects = tempfile::tempdir().expect("objects");
    let imported = import_local(&ImportPlan {
        repository_id: RepositoryId::from_bytes([9; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![PlannedFile {
            relative_path: "base.dat".into(),
            class: FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 2 * 1024 * 1024,
        output_staging_directory: objects.path().to_path_buf(),
        encryption: None,
    })
    .expect("import");
    let index = objects.path().join("mount.idx");
    mirage_index::compile_to_path(&imported.manifest, &index).expect("index");
    (source, objects, index)
}

fn provisioned_state() -> tempfile::TempDir {
    let state = tempfile::tempdir().expect("state");
    std::fs::create_dir(state.path().join("cache")).expect("cache dir");
    let database = mirage_db::Database::open(&state.path().join("control.db")).expect("db");
    let layout = mirage_cache::CacheLayout {
        page_size: mirage_types::ByteCount::from_u64(64 * 1024),
        slot_count: 64,
        db_journal_allowance: mirage_types::ByteCount::ZERO,
        filesystem_reserve: mirage_types::ByteCount::ZERO,
    };
    mirage_cache::ArenaShard::create(&state.path().join("cache/shard-0.bin"), layout)
        .expect("shard");
    database
        .register_cache_shard(mirage_db::CacheShardSpec {
            shard_id: 0,
            relative_path: "shard-0.bin".into(),
            page_size: layout.page_size,
            slot_count: 64,
        })
        .expect("register shard");
    state
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn managed_with_budget(
    index: &Path,
    state_root: &Path,
    provider_root: &Path,
    budget: u64,
) -> *mut MirageEngineHandle {
    let index16: Vec<u16> = index.to_string_lossy().encode_utf16().collect();
    let root16: Vec<u16> = state_root.to_string_lossy().encode_utf16().collect();
    let provider16: Vec<u16> = provider_root.to_string_lossy().encode_utf16().collect();
    let mut engine: *mut MirageEngineHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            mirage_engine_create_managed_local_provider(
                index16.as_ptr(),
                index16.len(),
                root16.as_ptr(),
                root16.len(),
                std::ptr::null(),
                0,
                budget,
                provider16.as_ptr(),
                provider16.len(),
                &mut engine,
            )
        },
        MirageStatus::Ok
    );
    assert!(!engine.is_null());
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    assert_eq!(
        unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
        MirageStatus::Ok
    );
    engine
}

fn open(engine: *mut MirageEngineHandle, path: &str) -> *mut MirageFileHandle {
    let path16 = utf16(path);
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    assert!(!file.is_null());
    file
}

fn create_write(engine: *mut MirageEngineHandle, path: &str, bytes: &[u8]) -> MirageStatus {
    let path16 = utf16(path);
    assert_eq!(
        unsafe { mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0) },
        MirageStatus::Ok
    );
    let file = open(engine, path);
    let mut transferred = 0usize;
    let status = unsafe { mirage_write(file, 0, bytes.as_ptr(), bytes.len(), &mut transferred) };
    if status == MirageStatus::Ok {
        assert_eq!(transferred, bytes.len());
        assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    }
    unsafe { mirage_file_close(file) };
    status
}

/// Writes `chunks` back to back, flushing between each so the file ends with
/// one sealed payload per chunk.
fn create_write_multi(engine: *mut MirageEngineHandle, path: &str, chunks: &[Vec<u8>]) {
    let path16 = utf16(path);
    assert_eq!(
        unsafe { mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0) },
        MirageStatus::Ok
    );
    let file = open(engine, path);
    let mut offset = 0u64;
    for chunk in chunks {
        let mut transferred = 0usize;
        assert_eq!(
            unsafe { mirage_write(file, offset, chunk.as_ptr(), chunk.len(), &mut transferred,) },
            MirageStatus::Ok
        );
        assert_eq!(transferred, chunk.len());
        assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
        offset += chunk.len() as u64;
    }
    unsafe { mirage_file_close(file) };
}

fn stats(engine: *mut MirageEngineHandle) -> MiragePublicationStats {
    let mut output = MiragePublicationStats {
        pending_payloads: 0,
        pending_bytes: 0,
        published_payloads: 0,
        published_bytes: 0,
        evicted_payloads: 0,
        integrity_refusals: 0,
        last_error_class: [0; 32],
    };
    assert_eq!(
        unsafe { mirage_engine_publication_stats(engine, &mut output) },
        MirageStatus::Ok
    );
    output
}

fn wait_for(engine: *mut MirageEngineHandle, predicate: impl Fn(&MiragePublicationStats) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if predicate(&stats(engine)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("stats never satisfied predicate: {:?}", stats(engine));
}

fn read_range(
    engine: *mut MirageEngineHandle,
    path: &str,
    offset: u64,
    len: usize,
) -> (MirageStatus, Vec<u8>) {
    let file = open(engine, path);
    let mut output = vec![0u8; len];
    let mut transferred = 0usize;
    let status = unsafe { mirage_read(file, offset, output.as_mut_ptr(), len, &mut transferred) };
    output.truncate(transferred);
    unsafe { mirage_file_close(file) };
    (status, output)
}

fn payload_names(journal_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(journal_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".payload"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn wait_payload_count(journal_dir: &Path, want: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while payload_names(journal_dir).len() < want && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        payload_names(journal_dir).len(),
        want,
        "journal payloads never reached {want}: {:?}",
        payload_names(journal_dir)
    );
}

fn evict(engine: *mut MirageEngineHandle, target: u64) -> (MirageStatus, u64) {
    let mut freed = 0u64;
    let mut blocked = 0u64;
    let status = unsafe { mirage_engine_evict_published(engine, target, &mut freed, &mut blocked) };
    (status, freed)
}

fn control_db(state_root: &Path) -> mirage_db::Database {
    mirage_db::Database::open(&state_root.join("control.db")).expect("control db")
}

fn volume_id() -> RepositoryId {
    RepositoryId::from_bytes([9; 16])
}

/// The deterministic content key of the directory-backend provider seam —
/// `ManagedProvider::local` derives the same value.
fn local_publication_key() -> Arc<mirage_crypto::aead::RepositoryKey> {
    Arc::new(mirage_crypto::aead::RepositoryKey::from_bytes(
        *blake3::hash(b"mirage-local-provider-publication-key/v1").as_bytes(),
    ))
}

fn published_payload_ids(db: &mirage_db::Database) -> Vec<[u8; 16]> {
    db.published_payloads_evictable(volume_id())
        .expect("evictable scan")
        .into_iter()
        .map(|(payload_id, ..)| payload_id)
        .collect()
}

/// Backend wrapper that counts concurrent `read_range` calls and adds a
/// fixed delay so the fetch window stays open long enough to observe.
struct CountingBackend {
    inner: mirage_ffi::directory_backend::DirectoryObjectBackend,
    inflight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    /// `read_range` start offsets, for asserting how one frame is fetched.
    starts: Arc<std::sync::Mutex<Vec<u64>>>,
    delay: Duration,
}

/// Observable backend counters handed to the test alongside the backend.
struct CountingMetrics {
    peak: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    starts: Arc<std::sync::Mutex<Vec<u64>>>,
}

impl CountingBackend {
    fn new(root: &Path, delay: Duration) -> (Self, CountingMetrics) {
        let metrics = CountingMetrics {
            peak: Arc::new(AtomicUsize::new(0)),
            calls: Arc::new(AtomicUsize::new(0)),
            starts: Arc::new(std::sync::Mutex::new(Vec::new())),
        };
        (
            Self {
                inner: mirage_ffi::directory_backend::DirectoryObjectBackend::new(
                    root,
                    volume_id(),
                )
                .expect("directory backend"),
                inflight: Arc::new(AtomicUsize::new(0)),
                peak: Arc::clone(&metrics.peak),
                calls: Arc::clone(&metrics.calls),
                starts: Arc::clone(&metrics.starts),
                delay,
            },
            metrics,
        )
    }
}

#[async_trait::async_trait]
impl mirage_backend::ObjectBackend for CountingBackend {
    fn capabilities(&self) -> mirage_backend::BackendCapabilities {
        self.inner.capabilities()
    }

    async fn read_range(
        &self,
        object: &mirage_backend::RemoteObjectRef,
        range: mirage_types::CheckedRange,
        class: mirage_backend::FetchClass,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<mirage_backend::BackendRead, mirage_backend::BackendError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut starts) = self.starts.lock() {
            starts.push(range.start());
        }
        let in_flight = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.peak.fetch_max(in_flight, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        let result = self.inner.read_range(object, range, class, cancel).await;
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        result
    }

    async fn put_immutable(
        &self,
        kind: mirage_backend::ObjectKind,
        source: mirage_backend::UploadSource,
        expected_hash: mirage_types::ContentHash,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<mirage_backend::RemoteObjectRef, mirage_backend::BackendError> {
        self.inner
            .put_immutable(kind, source, expected_hash, cancel)
            .await
    }

    async fn stat(
        &self,
        object: &mirage_backend::RemoteObjectRef,
    ) -> Result<mirage_backend::ObjectStat, mirage_backend::BackendError> {
        self.inner.stat(object).await
    }

    async fn enumerate_commits(
        &self,
        repository: RepositoryId,
    ) -> Result<Vec<mirage_backend::RemoteObjectRef>, mirage_backend::BackendError> {
        self.inner.enumerate_commits(repository).await
    }

    async fn delete_immutable(
        &self,
        object: &mirage_backend::RemoteObjectRef,
        proof: &mirage_backend::DeletionProof,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), mirage_backend::BackendError> {
        self.inner.delete_immutable(object, proof, cancel).await
    }

    async fn health(&self) -> mirage_types::BackendHealthState {
        self.inner.health().await
    }
}

#[test]
fn remote_read_fetches_frames_concurrently_and_verifies_bytes() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_with_budget(&index, state.path(), objects.path(), BUDGET);

    // 10 MiB spans three 4 MiB frames — enough to prove fetches overlap.
    let content: Vec<u8> = (0..10 * 1024 * 1024u64)
        .map(|i| (i.wrapping_mul(31).wrapping_add(7) & 0xFF) as u8)
        .collect();
    create_write(engine, "big.bin", &content);
    wait_for(engine, |s| s.published_payloads >= 1);
    let db = control_db(state.path());
    let payload_id = published_payload_ids(&db)[0];
    let (status, _) = evict(engine, u64::MAX);
    assert_eq!(status, MirageStatus::Ok);

    let (counting, metrics) = CountingBackend::new(objects.path(), Duration::from_millis(120));
    let store = mirage_ffi::publisher::RemotePayloadStore::new(
        Arc::new(counting),
        Arc::new(AtomicBool::new(true)),
        local_publication_key(),
        volume_id(),
    );
    let bytes = store
        .read(&db, volume_id(), &payload_id, 0, content.len())
        .expect("remote read")
        .expect("published payload");
    assert_eq!(bytes, content, "parallel frames reassembled out of order");
    assert!(
        metrics.peak.load(Ordering::SeqCst) > 1,
        "multi-frame read fetched serially (peak {})",
        metrics.peak.load(Ordering::SeqCst)
    );
    // Each ~4.3 MiB encoded frame splits into 1 MiB sub-ranges: three frames
    // → far more than three read_range calls, and more than one lands inside
    // the first frame's encoded region.
    assert!(
        metrics.calls.load(Ordering::SeqCst) > 6,
        "expected sub-range fetches, got {} calls",
        metrics.calls.load(Ordering::SeqCst)
    );
    let first_frame_end =
        mirage_ffi::publisher::frame_overhead() + mirage_ffi::publisher::PAYLOAD_FRAME_BYTES;
    let first_frame_calls = metrics
        .starts
        .lock()
        .expect("starts")
        .iter()
        .filter(|&&start| start < first_frame_end)
        .count();
    assert!(
        first_frame_calls > 1,
        "one read_range per frame: {first_frame_calls} calls in frame 0's region"
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn remote_read_rejects_a_corrupted_frame() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_with_budget(&index, state.path(), objects.path(), BUDGET);

    let content = vec![0x5Cu8; 6 * 1024 * 1024];
    create_write(engine, "corrupt.bin", &content);
    wait_for(engine, |s| s.published_payloads >= 1);
    let db = control_db(state.path());
    let payload_id = published_payload_ids(&db)[0];
    let record = db
        .payload_publication(volume_id(), &payload_id)
        .expect("publication")
        .expect("record");

    // Flip a byte inside the object's first frame: verification must fail
    // the read, never serve the tampered bytes.
    let object_path = objects.path().join(&record.provider_object_id);
    let mut object = std::fs::read(&object_path).expect("object bytes");
    let corrupt_at = usize::try_from(record.member_offset).expect("offset") + 64;
    object[corrupt_at] ^= 0xFF;
    std::fs::write(&object_path, object).expect("corrupt object");

    let store = mirage_ffi::publisher::RemotePayloadStore::new(
        Arc::new(
            mirage_ffi::directory_backend::DirectoryObjectBackend::new(objects.path(), volume_id())
                .expect("directory backend"),
        ),
        Arc::new(AtomicBool::new(true)),
        local_publication_key(),
        volume_id(),
    );
    let error = store
        .read(&db, volume_id(), &payload_id, 0, content.len())
        .expect_err("corrupted frame must fail");
    assert_eq!(
        error.kind,
        mirage_types::MirageErrorKind::IntegrityMismatch,
        "corrupted frame yielded {error:?}"
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn remote_read_rejects_a_corrupted_subrange() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_with_budget(&index, state.path(), objects.path(), BUDGET);

    let content = vec![0x7Eu8; 6 * 1024 * 1024];
    create_write(engine, "corrupt-range.bin", &content);
    wait_for(engine, |s| s.published_payloads >= 1);
    let db = control_db(state.path());
    let payload_id = published_payload_ids(&db)[0];
    let record = db
        .payload_publication(volume_id(), &payload_id)
        .expect("publication")
        .expect("record");

    // Flip a byte two MiB into frame 0's encoded region — inside the third
    // sub-range, not the first bytes the fetch touches.
    let object_path = objects.path().join(&record.provider_object_id);
    let mut object = std::fs::read(&object_path).expect("object bytes");
    let corrupt_at = usize::try_from(record.member_offset).expect("offset") + 2 * 1024 * 1024;
    object[corrupt_at] ^= 0xFF;
    std::fs::write(&object_path, object).expect("corrupt object");

    let store = mirage_ffi::publisher::RemotePayloadStore::new(
        Arc::new(
            mirage_ffi::directory_backend::DirectoryObjectBackend::new(objects.path(), volume_id())
                .expect("directory backend"),
        ),
        Arc::new(AtomicBool::new(true)),
        local_publication_key(),
        volume_id(),
    );
    let error = store
        .read(&db, volume_id(), &payload_id, 0, content.len())
        .expect_err("corrupted sub-range must fail");
    assert_eq!(
        error.kind,
        mirage_types::MirageErrorKind::IntegrityMismatch,
        "corrupted sub-range yielded {error:?}"
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn evicted_read_serves_bytes_first_and_restages_in_background() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_with_budget(&index, state.path(), objects.path(), BUDGET);
    let journal = state.path().join("journal");

    let content: Vec<u8> = (0..512 * 1024usize).map(|i| (i % 251) as u8).collect();
    create_write(engine, "cold.bin", &content);
    wait_for(engine, |s| s.published_payloads >= 1);
    let (status, freed) = evict(engine, u64::MAX);
    assert_eq!(status, MirageStatus::Ok);
    assert!(freed >= content.len() as u64);
    assert!(payload_names(&journal).is_empty());

    // The read returns correct bytes immediately; the whole payload is
    // re-staged in the background shortly after.
    let (status, bytes) = read_range(engine, "cold.bin", 0, content.len());
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, content);
    wait_payload_count(&journal, 1, Duration::from_secs(15));
    // Resident again: the bytes also read back identically.
    let (status, bytes) = read_range(engine, "cold.bin", 0, content.len());
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, content);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn cold_read_lookahead_restages_the_next_evicted_payloads() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_with_budget(&index, state.path(), objects.path(), BUDGET);
    let journal = state.path().join("journal");

    // Three seals → three contiguous payloads on one inode.
    let chunks: Vec<Vec<u8>> = (0..3)
        .map(|chunk| vec![0xA0u8 + chunk as u8; 256 * 1024])
        .collect();
    create_write_multi(engine, "stream.bin", &chunks);
    wait_for(engine, |s| s.published_payloads >= 3);
    let (status, freed) = evict(engine, u64::MAX);
    assert_eq!(status, MirageStatus::Ok);
    assert!(freed >= 3 * 256 * 1024);
    assert!(payload_names(&journal).is_empty());

    // Reading the first payload must look ahead and re-stage the rest.
    let (status, bytes) = read_range(engine, "stream.bin", 0, 64 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0xA0));
    wait_payload_count(&journal, 3, Duration::from_secs(20));
    // Every slice now serves resident bytes — spot check all three payloads.
    for (i, chunk) in chunks.iter().enumerate() {
        let (status, bytes) = read_range(engine, "stream.bin", (i * 256 * 1024) as u64, 64 * 1024);
        assert_eq!(status, MirageStatus::Ok);
        assert!(bytes.iter().all(|b| *b == chunk[0]));
    }
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
