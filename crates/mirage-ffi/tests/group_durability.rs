#![allow(unsafe_code)]

//! Group-commit durability (`MIRAGE_DURABILITY` unset ⇒ managed engines run
//! synchronous=NORMAL + writer barriers): close enqueues seals without
//! waiting, flush/drain barrier explicitly, and deleted files never leave a
//! payload that committed state could reference missing.

use std::path::{Path, PathBuf};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageStatus, mirage_engine_abandon_for_tests,
    mirage_engine_create_managed, mirage_engine_destroy, mirage_engine_dirty_used_for_tests,
    mirage_engine_mark_mounted, mirage_engine_quiesce, mirage_file_close, mirage_flush,
    mirage_lookup, mirage_namespace_create, mirage_namespace_delete, mirage_read, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const VOLUME: RepositoryId = RepositoryId::from_bytes([9; 16]);

fn build_index() -> (tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), b"seed").expect("seed file");
    let objects = tempfile::tempdir().expect("objects");
    let imported = import_local(&ImportPlan {
        repository_id: VOLUME,
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
    (objects, index)
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `mount=false` keeps the test in the recovering-owner state an abandoned
/// engine leaves behind.
fn managed_engine(
    index: &Path,
    state_root: &Path,
    budget: u64,
    mark_mounted: bool,
) -> *mut MirageEngineHandle {
    let index16: Vec<u16> = index.to_string_lossy().encode_utf16().collect();
    let root16: Vec<u16> = state_root.to_string_lossy().encode_utf16().collect();
    let mut engine: *mut MirageEngineHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            mirage_engine_create_managed(
                index16.as_ptr(),
                index16.len(),
                root16.as_ptr(),
                root16.len(),
                std::ptr::null(),
                0,
                budget,
                &mut engine,
            )
        },
        MirageStatus::Ok
    );
    if mark_mounted {
        assert_eq!(
            unsafe { mirage_engine_mark_mounted(engine) },
            MirageStatus::Ok
        );
    }
    engine
}

fn create_and_open(engine: *mut MirageEngineHandle, path: &str) -> *mut MirageFileHandle {
    let path16 = utf16(path);
    assert_eq!(
        unsafe { mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0) },
        MirageStatus::Ok
    );
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    assert!(!file.is_null());
    file
}

fn write(file: *mut MirageFileHandle, offset: u64, bytes: &[u8]) -> MirageStatus {
    let mut transferred = 0usize;
    unsafe { mirage_write(file, offset, bytes.as_ptr(), bytes.len(), &mut transferred) }
}

fn read_exact(file: *mut MirageFileHandle, offset: u64, len: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; len];
    let mut transferred = 0usize;
    assert_eq!(
        unsafe { mirage_read(file, offset, buffer.as_mut_ptr(), len, &mut transferred) },
        MirageStatus::Ok
    );
    buffer.truncate(transferred);
    buffer
}

fn payload_files(journal_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(journal_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".payload"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// (a) Group-mode close returns after enqueue, not seal — an immediate
/// reopen must still read the bytes through the live extent map.
#[test]
fn async_close_then_immediate_read_sees_the_bytes() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20, true);
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    let file = create_and_open(engine, "\\close.bin");
    assert_eq!(write(file, 0, &payload), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);

    let again16 = utf16("\\close.bin");
    let mut again: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, again16.as_ptr(), again16.len(), &mut again) },
        MirageStatus::Ok
    );
    assert_eq!(read_exact(again, 0, payload.len()), payload);
    assert_eq!(unsafe { mirage_file_close(again) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

/// (b) Deleting a file whose seal is still in flight drops the payload, and
/// the dirty ledger returns to its pre-write level after the drain.
#[test]
fn delete_during_queued_seal_discards_the_payload() {
    // Strict mode drains on close — the async-discard path being exercised
    // here only exists under group commit.
    if std::env::var("MIRAGE_DURABILITY").as_deref() == Ok("strict") {
        return;
    }
    unsafe { std::env::set_var("MIRAGE_TEST_SEAL_DELAY_MS", "500") };
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let journal_dir = state_root.path().join("journal");
    let engine = managed_engine(&index, state_root.path(), 64 << 20, true);
    let baseline = unsafe { mirage_engine_dirty_used_for_tests(engine) };

    let file = create_and_open(engine, "\\gone.bin");
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 199) as u8).collect();
    assert_eq!(write(file, 0, &payload), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    // The sealer is sleeping inside the seal delay — delete lands while the
    // job is in flight but unstarted.
    let del = utf16("\\gone.bin");
    assert_eq!(
        unsafe { mirage_namespace_delete(engine, del.as_ptr(), del.len()) },
        MirageStatus::Ok
    );
    assert_eq!(
        unsafe { mirage_engine_quiesce(engine, 10_000) },
        MirageStatus::Ok
    );
    assert_eq!(
        payload_files(&journal_dir).len(),
        0,
        "deleted file's payload must not survive"
    );
    assert_eq!(
        unsafe { mirage_engine_dirty_used_for_tests(engine) },
        baseline,
        "discarded segment bytes must come off the dirty ledger"
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
    unsafe { std::env::remove_var("MIRAGE_TEST_SEAL_DELAY_MS") };
}

/// (c) mirage_flush drains the seal and barriers — from a fresh Database the
/// committed extent head and the bumped barrier epoch are both visible.
#[test]
fn flush_makes_extents_durable_before_returning() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20, true);
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 211) as u8).collect();
    let file = create_and_open(engine, "\\durable.bin");
    assert_eq!(write(file, 0, &payload), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);

    let db = mirage_db::Database::open(&state_root.path().join("control.db"))
        .expect("reopen control.db");
    let inode = db
        .namespace_resolve_components(VOLUME, &["durable.bin".to_string()])
        .expect("resolve")
        .expect("inode exists");
    assert!(
        db.extent_head(VOLUME, inode)
            .expect("extent head query")
            .is_some(),
        "flushed extents must be committed before mirage_flush returns"
    );
    // Strict mode fsyncs every commit — the explicit barrier only runs under
    // group commit.
    if std::env::var("MIRAGE_DURABILITY").as_deref() != Ok("strict") {
        assert!(
            db.durability_barrier_seq().unwrap() > 0,
            "group-commit barrier must run inside mirage_flush"
        );
    }
    drop(db);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

/// (d) Crash after an async close: the queued seal may or may not land —
/// the file reads back either complete or empty, never a partial byte mix,
/// and the mount sweep removes the orphan payload.
#[test]
fn crash_after_async_close_is_atomic_and_swept() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20, true);
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 233) as u8).collect();
    let file = create_and_open(engine, "\\crash.bin");
    assert_eq!(write(file, 0, &payload), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    // Crash: no drain, no barrier — whatever the sealer committed stands.
    assert_eq!(
        unsafe { mirage_engine_abandon_for_tests(engine) },
        MirageStatus::Ok
    );

    let engine = managed_engine(&index, state_root.path(), 64 << 20, false);
    let path16 = utf16("\\crash.bin");
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    let content = read_exact(file, 0, payload.len());
    assert!(
        content == payload || content.is_empty(),
        "crash must leave the file full-or-empty, got {} bytes",
        content.len()
    );
    let committed = content == payload;
    assert_eq!(
        payload_files(&state_root.path().join("journal")).len(),
        usize::from(committed),
        "uncommitted payload must be swept, committed payload kept"
    );
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
