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

#[test]
fn failed_seals_stay_failed_until_ordered_retry_is_durable() {
    let (_objects, index) = build_index();
    let root = tempfile::tempdir().unwrap();
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let file = create_and_open(engine, "\\retry.bin");
    let connection = rusqlite::Connection::open(root.path().join("control.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_seal BEFORE INSERT ON byte_extent_heads WHEN NEW.eof = 5 BEGIN SELECT RAISE(ABORT, 'injected seal failure'); END;").unwrap();
    assert_eq!(write(file, 0, b"first"), MirageStatus::Ok);
    assert_eq!(write(file, 4096, b"second"), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::IoError);
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::IoError);
    // A failed retry cannot commit the newer snapshot around the older job.
    let heads: i64 = connection
        .query_row("SELECT COUNT(*) FROM byte_extent_heads", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(heads, 0);
    assert_eq!(read_exact(file, 0, 5), b"first");
    assert_eq!(read_exact(file, 4096, 6), b"second");
    connection.execute_batch("DROP TRIGGER fail_seal").unwrap();
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
    let reopened = managed_engine(&index, root.path(), 64 << 20, true);
    let path = utf16("\\retry.bin");
    let mut file = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(reopened, path.as_ptr(), path.len(), &mut file) },
        MirageStatus::Ok
    );
    assert_eq!(read_exact(file, 0, 5), b"first");
    assert_eq!(read_exact(file, 4096, 6), b"second");
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(reopened) }, MirageStatus::Ok);
}

#[test]
fn timestamp_set_after_failed_enqueue_survives_close_and_retry() {
    let (_objects, index) = build_index();
    let root = tempfile::tempdir().unwrap();
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let file = create_and_open(engine, "\\stamp.bin");
    let connection = rusqlite::Connection::open(root.path().join("control.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_seal BEFORE INSERT ON byte_extent_heads BEGIN SELECT RAISE(ABORT, 'injected seal failure'); END;").unwrap();
    assert_eq!(write(file, 0, b"preserved"), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::IoError);
    assert_eq!(
        unsafe { mirage_ffi::mirage_set_times(file, 0, 123456789) },
        MirageStatus::Ok
    );
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(
        unsafe { mirage_ffi::mirage_flush_volume(engine) },
        MirageStatus::IoError
    );
    connection.execute_batch("DROP TRIGGER fail_seal").unwrap();
    assert_eq!(
        unsafe { mirage_ffi::mirage_flush_volume(engine) },
        MirageStatus::Ok
    );
    let modified: i64 = connection
        .query_row("SELECT i.modified_ns FROM inodes i JOIN dirents d ON i.volume_id=d.volume_id AND i.inode=d.child_inode WHERE d.display_name='stamp.bin'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(modified, 123456789);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn null_zero_write_and_volume_barrier_are_supported() {
    let (_objects, index) = build_index();
    let root = tempfile::tempdir().unwrap();
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let a = create_and_open(engine, "\\a.bin");
    let b = create_and_open(engine, "\\b.bin");
    let mut transferred = usize::MAX;
    assert_eq!(
        unsafe { mirage_write(a, 0, std::ptr::null(), 0, &mut transferred) },
        MirageStatus::Ok
    );
    assert_eq!(transferred, 0);
    assert_eq!(write(a, 0, b"a"), MirageStatus::Ok);
    assert_eq!(write(b, 0, b"b"), MirageStatus::Ok);
    assert_eq!(
        unsafe { mirage_ffi::mirage_flush_volume(engine) },
        MirageStatus::Ok
    );
    assert_eq!(unsafe { mirage_file_close(a) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(b) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn rejected_unlink_preserves_open_handle_bytes_and_share_state() {
    let (_objects, index) = build_index();
    let root = tempfile::tempdir().unwrap();
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let file = create_and_open(engine, "\\guard.bin");
    assert_eq!(write(file, 0, b"accepted"), MirageStatus::Ok);
    let connection = rusqlite::Connection::open(root.path().join("control.db")).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_unlink BEFORE DELETE ON dirents WHEN OLD.display_name = 'guard.bin' BEGIN SELECT RAISE(ABORT, 'injected unlink failure'); END;").unwrap();
    let path = utf16("\\guard.bin");
    assert_eq!(
        unsafe { mirage_namespace_delete(engine, path.as_ptr(), path.len()) },
        MirageStatus::IoError
    );
    assert_eq!(read_exact(file, 0, 8), b"accepted");
    let mut again = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path.as_ptr(), path.len(), &mut again) },
        MirageStatus::Ok
    );
    assert_eq!(unsafe { mirage_file_close(again) }, MirageStatus::Ok);
    connection
        .execute_batch("DROP TRIGGER fail_unlink")
        .unwrap();
    assert_eq!(
        unsafe { mirage_namespace_delete(engine, path.as_ptr(), path.len()) },
        MirageStatus::Ok
    );
    assert_eq!(write(file, 8, b" after unlink"), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    assert_eq!(read_exact(file, 0, 21), b"accepted after unlink");
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    let pending: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM inodes WHERE unlink_pending=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    assert_eq!(unsafe { mirage_engine_dirty_used_for_tests(engine) }, 0);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
#[ignore = "child-process fixture invoked by abrupt_unlink_recovery"]
fn storage_crash_child() {
    let root = PathBuf::from(std::env::var_os("MIRAGE_STORAGE_CHILD_ROOT").unwrap());
    let index = PathBuf::from(std::env::var_os("MIRAGE_STORAGE_CHILD_INDEX").unwrap());
    let engine = managed_engine(&index, &root, 64 << 20, true);
    let persisted = create_and_open(engine, "\\persisted.bin");
    assert_eq!(write(persisted, 0, b"durably flushed"), MirageStatus::Ok);
    let deleted = create_and_open(engine, "\\deleted.bin");
    assert_eq!(write(deleted, 0, b"still open"), MirageStatus::Ok);
    let path = utf16("\\deleted.bin");
    assert_eq!(
        unsafe { mirage_namespace_delete(engine, path.as_ptr(), path.len()) },
        MirageStatus::Ok
    );
    assert_eq!(write(deleted, 10, b" and writable"), MirageStatus::Ok);
    assert_eq!(
        unsafe { mirage_ffi::mirage_flush_volume(engine) },
        MirageStatus::Ok
    );
    std::fs::write(root.join("child-ready"), b"flushed").unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[test]
fn abrupt_unlink_recovery_keeps_flushed_files_and_reclaims_tombstones() {
    let (_objects, index) = build_index();
    let root = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "storage_crash_child", "--ignored", "--nocapture"])
        .env("MIRAGE_STORAGE_CHILD_ROOT", root.path())
        .env("MIRAGE_STORAGE_CHILD_INDEX", &index)
        .stdout(std::fs::File::create(root.path().join("child.stdout")).unwrap())
        .stderr(std::fs::File::create(root.path().join("child.stderr")).unwrap())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !root.path().join("child-ready").is_file() {
        if child.try_wait().unwrap().is_some() || std::time::Instant::now() >= deadline {
            let _ = child.kill();
            panic!(
                "storage child failed: {}",
                std::fs::read_to_string(root.path().join("child.stderr")).unwrap()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().unwrap(); // Actual process termination; no abandon/drain helper.
    child.wait().unwrap();
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let path = utf16("\\persisted.bin");
    let mut file = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path.as_ptr(), path.len(), &mut file) },
        MirageStatus::Ok
    );
    assert_eq!(read_exact(file, 0, 15), b"durably flushed");
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    let path = utf16("\\deleted.bin");
    assert_eq!(
        unsafe { mirage_lookup(engine, path.as_ptr(), path.len(), &mut file) },
        MirageStatus::NotFound
    );
    assert_eq!(payload_files(&root.path().join("journal")).len(), 1);
    assert_eq!(unsafe { mirage_engine_dirty_used_for_tests(engine) }, 15);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn rename_replacement_policy_preserves_collision_then_replaces() {
    let (_objects, index) = build_index();
    let root = tempfile::tempdir().unwrap();
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let a = create_and_open(engine, "\\a.bin");
    let b = create_and_open(engine, "\\b.bin");
    assert_eq!(write(a, 0, b"source"), MirageStatus::Ok);
    assert_eq!(write(b, 0, b"victim"), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(a) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(b) }, MirageStatus::Ok);
    let from = utf16("\\a.bin");
    let to = utf16("\\b.bin");
    assert_eq!(
        unsafe {
            mirage_ffi::mirage_namespace_rename_ex(
                engine,
                from.as_ptr(),
                from.len(),
                to.as_ptr(),
                to.len(),
                0,
            )
        },
        MirageStatus::Conflict
    );
    let mut b = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, to.as_ptr(), to.len(), &mut b) },
        MirageStatus::Ok
    );
    assert_eq!(read_exact(b, 0, 6), b"victim");
    assert_eq!(unsafe { mirage_file_close(b) }, MirageStatus::Ok);
    assert_eq!(
        unsafe {
            mirage_ffi::mirage_namespace_rename_ex(
                engine,
                from.as_ptr(),
                from.len(),
                to.as_ptr(),
                to.len(),
                1,
            )
        },
        MirageStatus::Ok
    );
    assert_eq!(
        unsafe { mirage_lookup(engine, to.as_ptr(), to.len(), &mut b) },
        MirageStatus::Ok
    );
    assert_eq!(read_exact(b, 0, 6), b"source");
    assert_eq!(unsafe { mirage_file_close(b) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[cfg(windows)]
#[test]
fn locked_deleted_payload_stays_charged_until_compaction_retry() {
    use std::os::windows::fs::OpenOptionsExt;
    let (_objects, index) = build_index();
    let root = tempfile::tempdir().unwrap();
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let file = create_and_open(engine, "\\locked.bin");
    assert_eq!(write(file, 0, b"locked payload"), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    let payload = root
        .path()
        .join("journal")
        .join(&payload_files(&root.path().join("journal"))[0]);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&payload)
        .unwrap();
    let path = utf16("\\locked.bin");
    assert_eq!(
        unsafe { mirage_namespace_delete(engine, path.as_ptr(), path.len()) },
        MirageStatus::Ok
    );
    assert_eq!(unsafe { mirage_engine_dirty_used_for_tests(engine) }, 14);
    assert_eq!(
        unsafe { mirage_ffi::mirage_engine_compact(engine) },
        MirageStatus::Ok
    );
    assert_eq!(unsafe { mirage_engine_dirty_used_for_tests(engine) }, 14);
    assert!(payload.is_file());
    drop(lock);
    assert_eq!(
        unsafe { mirage_ffi::mirage_engine_compact(engine) },
        MirageStatus::Ok
    );
    assert_eq!(unsafe { mirage_engine_dirty_used_for_tests(engine) }, 0);
    assert!(!payload.exists());
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn creation_only_timestamp_preserves_reopened_modification_time() {
    let (_objects, index) = build_index();
    let root = tempfile::tempdir().unwrap();
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let file = create_and_open(engine, "\\created.bin");
    assert_eq!(
        unsafe { mirage_ffi::mirage_set_times(file, 0, 123456789) },
        MirageStatus::Ok
    );
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
    let engine = managed_engine(&index, root.path(), 64 << 20, true);
    let path = utf16("\\created.bin");
    let mut file = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path.as_ptr(), path.len(), &mut file) },
        MirageStatus::Ok
    );
    assert_eq!(
        unsafe { mirage_ffi::mirage_set_times(file, 987654321, 0) },
        MirageStatus::Ok
    );
    let mut info = mirage_ffi::MirageFileInfo {
        stable_index: 0,
        size: 0,
        directory: 0,
        reserved: [0; 7],
        created_ns: 0,
        modified_ns: 0,
    };
    assert_eq!(
        unsafe { mirage_ffi::mirage_file_stat(file, &mut info) },
        MirageStatus::Ok
    );
    assert_eq!(info.created_ns, 987654321);
    assert_eq!(info.modified_ns, 123456789);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
