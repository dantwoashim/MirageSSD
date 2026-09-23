#![allow(unsafe_code)]

//! Write-behind segment-writer gates: contiguous writes seal into one
//! payload/extent per segment, seal points (flush, close, truncate, idle),
//! crash-drop honesty, and explicit timestamps.

use std::path::{Path, PathBuf};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageFileInfo, MirageStatus,
    mirage_engine_abandon_for_tests, mirage_engine_create_managed,
    mirage_engine_create_managed_drive_at, mirage_engine_destroy, mirage_engine_mark_mounted,
    mirage_enumerate, mirage_file_close, mirage_file_stat, mirage_flush, mirage_lookup,
    mirage_namespace_create, mirage_read, mirage_set_times, mirage_truncate, mirage_write,
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

fn managed_engine(index: &Path, state_root: &Path, budget: u64) -> *mut MirageEngineHandle {
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
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    engine
}

fn create_and_open(engine: *mut MirageEngineHandle, path: &str) -> *mut MirageFileHandle {
    let path16 = utf16(path);
    assert_eq!(
        unsafe { mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0) },
        MirageStatus::Ok
    );
    open_existing(engine, path)
}

fn open_existing(engine: *mut MirageEngineHandle, path: &str) -> *mut MirageFileHandle {
    let path16 = utf16(path);
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

fn stat(file: *mut MirageFileHandle) -> MirageFileInfo {
    let mut info = MirageFileInfo {
        stable_index: 0,
        size: 0,
        directory: 0,
        reserved: [0; 7],
        created_ns: 0,
        modified_ns: 0,
    };
    assert_eq!(
        unsafe { mirage_file_stat(file, &mut info) },
        MirageStatus::Ok
    );
    info
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

#[test]
fn sequential_writes_seal_to_one_extent_and_one_payload() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = create_and_open(engine, "big.bin");
    let chunk = vec![0x5au8; 65536];
    for block in 0..256u64 {
        assert_eq!(write(file, block * 65536, &chunk), MirageStatus::Ok);
    }
    // Before flush: live size, byte-exact reads, no durable version yet.
    assert_eq!(stat(file).size, 16 << 20);
    assert_eq!(read_exact(file, 65536, 65536), vec![0x5au8; 65536]);
    let db = mirage_db::Database::open(&state_root.path().join("control.db")).expect("db");
    let inode = unsafe { &*file }.inode.expect("inode");
    assert_eq!(
        db.extent_latest_version(VOLUME, inode).expect("versions"),
        None,
        "unsealed writes must not commit extent versions"
    );
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    let version = db
        .extent_latest_version(VOLUME, inode)
        .expect("versions")
        .expect("head");
    let extents = db.extents_at(VOLUME, inode, version).expect("extents");
    let dirty_extents: Vec<_> = extents
        .iter()
        .filter(|extent| extent.kind == mirage_db::ExtentKind::Dirty)
        .collect();
    assert_eq!(dirty_extents.len(), 1, "16 MiB must seal as one extent");
    assert_eq!(dirty_extents[0].length, 16 << 20);
    let journal_dir = state_root.path().join("journal");
    assert_eq!(payload_files(&journal_dir).len(), 1);
    let dirty = unsafe { &*engine }.dirty.as_ref().expect("dirty ledger");
    assert_eq!(
        dirty.used.load(std::sync::atomic::Ordering::Acquire),
        16 << 20
    );
    drop(db);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn non_contiguous_writes_stay_split_and_holes_read_zero() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = create_and_open(engine, "sparse.bin");
    assert_eq!(write(file, 0, &[0x11; 1 << 20]), MirageStatus::Ok);
    assert_eq!(write(file, 10 << 20, &[0x22; 1 << 20]), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    let db = mirage_db::Database::open(&state_root.path().join("control.db")).expect("db");
    let inode = unsafe { &*file }.inode.expect("inode");
    let version = db
        .extent_latest_version(VOLUME, inode)
        .expect("versions")
        .expect("head");
    let extents = db.extents_at(VOLUME, inode, version).expect("extents");
    let dirty_extents: Vec<_> = extents
        .iter()
        .filter(|extent| extent.kind == mirage_db::ExtentKind::Dirty)
        .collect();
    assert_eq!(
        dirty_extents.len(),
        2,
        "disjoint writes seal as two extents"
    );
    drop(db);
    assert_eq!(read_exact(file, 0, 4), vec![0x11; 4]);
    assert_eq!(read_exact(file, 10 << 20, 4), vec![0x22; 4]);
    assert_eq!(read_exact(file, 5 << 20, 4), vec![0u8; 4]);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn close_makes_writes_durable_across_engines() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = create_and_open(engine, "durable.bin");
    let payload: Vec<u8> = (0..1 << 20).map(|i| (i % 251) as u8).collect();
    assert_eq!(write(file, 0, &payload), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);

    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = open_existing(engine, "durable.bin");
    assert_eq!(read_exact(file, 0, 1 << 20), payload);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn abandoned_engine_loses_only_the_unsealed_tail() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = create_and_open(engine, "crash.bin");
    assert_eq!(write(file, 0, &[0x77; 4096]), MirageStatus::Ok);
    // No flush, no close: the segment is unsealed when the engine dies.
    assert_eq!(
        unsafe { mirage_engine_abandon_for_tests(engine) },
        MirageStatus::Ok
    );
    let journal_dir = state_root.path().join("journal");
    assert_eq!(
        payload_files(&journal_dir).len(),
        1,
        "unsealed payload file remains for the sweep"
    );

    // The crashed owner record makes the new engine Recovering — create
    // without the mark_mounted assertion, then read through the sweep.
    let index16: Vec<u16> = index.to_string_lossy().encode_utf16().collect();
    let root16: Vec<u16> = state_root.path().to_string_lossy().encode_utf16().collect();
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
                64 << 20,
                &mut engine,
            )
        },
        MirageStatus::Ok
    );
    assert_eq!(
        payload_files(&journal_dir).len(),
        0,
        "mount sweep removes the unsealed payload"
    );
    let file = open_existing(engine, "crash.bin");
    // The durable EOF is the pre-write one (empty file); no garbage bytes.
    assert_eq!(stat(file).size, 0);
    assert_eq!(read_exact(file, 0, 4096), Vec::<u8>::new());
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn truncate_after_write_seals_then_clips() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = create_and_open(engine, "clip.bin");
    let payload: Vec<u8> = (0..1 << 20).map(|i| (i % 253) as u8).collect();
    assert_eq!(write(file, 0, &payload), MirageStatus::Ok);
    assert_eq!(
        unsafe { mirage_truncate(file, 512 << 10) },
        MirageStatus::Ok
    );
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    assert_eq!(stat(file).size, 512 << 10);
    assert_eq!(read_exact(file, 0, 512 << 10), payload[..512 << 10]);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn explicit_mtime_survives_writes() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = create_and_open(engine, "times.bin");
    let explicit: i64 = 1_600_000_000_000_000_000;
    assert_eq!(
        unsafe { mirage_set_times(file, 0, explicit) },
        MirageStatus::Ok
    );
    assert_eq!(write(file, 0, &[1; 4096]), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    let file = open_existing(engine, "times.bin");
    assert_eq!(stat(file).modified_ns, explicit);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);

    // Without an explicit mtime, writes advance the stamp.
    let file = create_and_open(engine, "times2.bin");
    assert_eq!(write(file, 0, &[2; 4096]), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    let file = open_existing(engine, "times2.bin");
    let created = stat(file).created_ns;
    assert!(stat(file).modified_ns >= created && created > 0);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn enumerate_reports_in_memory_size_while_segment_is_open() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = create_and_open(engine, "live.bin");
    assert_eq!(write(file, 0, &[9; 1 << 20]), MirageStatus::Ok);
    // Enumerate the root while the segment is still open (no flush/close).
    let root = open_existing(engine, "");
    let mut seen_size = 0u64;
    let callback: mirage_ffi::MirageEnumerateCallback = {
        unsafe extern "C" fn cb(
            context: *mut core::ffi::c_void,
            name: *const u16,
            name_len: usize,
            info: MirageFileInfo,
        ) -> u8 {
            let name = String::from_utf16(unsafe { std::slice::from_raw_parts(name, name_len) })
                .unwrap_or_default();
            if name == "live.bin" {
                unsafe { *(context as *mut u64) = info.size };
            }
            1
        }
        cb
    };
    assert_eq!(
        unsafe {
            mirage_enumerate(
                root,
                std::ptr::null(),
                0,
                256,
                &mut seen_size as *mut u64 as *mut core::ffi::c_void,
                Some(callback),
            )
        },
        MirageStatus::Ok
    );
    assert_eq!(seen_size, 1 << 20, "enumerate must report the live size");
    assert_eq!(unsafe { mirage_file_close(root) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

/// Deadlock gate for the extents → open lock order: an idle segment on one
/// file must seal while another file is actively writing — the sealer's
/// idle tick and the writer's append must never wait on each other.
#[test]
fn idle_seal_during_active_writes_never_deadlocks() {
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watchdog = std::sync::Arc::clone(&done);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        if !watchdog.load(std::sync::atomic::Ordering::Acquire) {
            panic!("write path deadlocked — test did not finish in 30 s");
        }
    });

    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let a = create_and_open(engine, "a.bin");
    let b = create_and_open(engine, "b.bin");
    assert_eq!(write(a, 0, &[0xAA; 1 << 20]), MirageStatus::Ok);
    // Let A go idle so the sealer's tick wants to enqueue it.
    std::thread::sleep(std::time::Duration::from_millis(2600));
    // Keep writing to B for ~4 s — every chunk must stay Ok. Writes are
    // paced so the dirty budget is never the bottleneck; the point is
    // lock contention, not throughput.
    let chunk = vec![0xBBu8; 65536];
    let mut offset = 0u64;
    for _ in 0..40 {
        assert_eq!(write(b, offset, &chunk), MirageStatus::Ok);
        offset += 65536;
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(unsafe { mirage_file_close(a) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(b) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
    done.store(true, std::sync::atomic::Ordering::Release);
}

/// Engine whose journal payloads live in journal_root (the user's cache
/// disk) instead of <state_root>/journal.
fn managed_engine_at(
    index: &Path,
    state_root: &Path,
    journal_root: &Path,
    budget: u64,
) -> *mut MirageEngineHandle {
    let index16: Vec<u16> = index.to_string_lossy().encode_utf16().collect();
    let root16: Vec<u16> = state_root.to_string_lossy().encode_utf16().collect();
    let journal16: Vec<u16> = journal_root.to_string_lossy().encode_utf16().collect();
    let mut engine: *mut MirageEngineHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            mirage_engine_create_managed_drive_at(
                index16.as_ptr(),
                index16.len(),
                root16.as_ptr(),
                root16.len(),
                std::ptr::null(),
                0,
                budget,
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                journal16.as_ptr(),
                journal16.len(),
                &mut engine,
            )
        },
        MirageStatus::Ok
    );
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    engine
}

#[test]
fn custom_journal_root_holds_the_payloads_and_survives_remount() {
    let (_objects, index) = build_index();
    let state = tempfile::tempdir().expect("state");
    let cache = tempfile::tempdir().expect("cache disk");
    let journal_root = cache.path().join("MirageSSD").join("journal");
    std::fs::create_dir_all(&journal_root).expect("journal root");
    let data: Vec<u8> = (0..(3u32 << 20)).map(|i| (i % 251) as u8).collect();

    let engine = managed_engine_at(&index, state.path(), &journal_root, 64 << 20);
    let file = create_and_open(engine, "\\on-d.bin");
    assert_eq!(write(file, 0, &data), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(
        payload_files(&journal_root).len(),
        1,
        "payload lands on the cache disk"
    );
    assert!(
        payload_files(&state.path().join("journal")).is_empty(),
        "nothing is written under the state root"
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);

    // Remount with the same journal root: the bytes are still there.
    let engine = managed_engine_at(&index, state.path(), &journal_root, 64 << 20);
    let file = open_existing(engine, "\\on-d.bin");
    assert_eq!(stat(file).size, data.len() as u64);
    assert_eq!(read_exact(file, 0, data.len()), data);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(
        payload_files(&journal_root).len(),
        1,
        "remount sweep keeps referenced payloads"
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

/// Raw FFI read throughput (no WinFsp): 256 MiB written, then read back in
/// 64 KiB requests like the cache manager issues. Prints MB/s; run with
/// --ignored --nocapture.
#[test]
#[ignore]
fn ffi_read_throughput_probe() {
    let (_objects, index) = build_index();
    let state = tempfile::tempdir().expect("state");
    let engine = managed_engine(&index, state.path(), 1 << 30);
    let total: usize = 256 << 20;
    let chunk: usize = 1 << 20;
    let data: Vec<u8> = (0..chunk as u32).map(|i| (i % 253) as u8).collect();
    let file = create_and_open(engine, "\\probe.bin");
    let started = std::time::Instant::now();
    for offset in (0..total).step_by(chunk) {
        assert_eq!(write(file, offset as u64, &data), MirageStatus::Ok);
    }
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    let write_secs = started.elapsed().as_secs_f64();
    let read_chunk: usize = 64 << 10;
    let mut buffer = vec![0u8; read_chunk];
    let started = std::time::Instant::now();
    for offset in (0..total).step_by(read_chunk) {
        let mut transferred = 0usize;
        assert_eq!(
            unsafe {
                mirage_read(
                    file,
                    offset as u64,
                    buffer.as_mut_ptr(),
                    read_chunk,
                    &mut transferred,
                )
            },
            MirageStatus::Ok
        );
        assert_eq!(transferred, read_chunk);
    }
    let read_secs = started.elapsed().as_secs_f64();
    eprintln!(
        "FFI write {:.0} MB/s, read (64 KiB requests) {:.0} MB/s",
        (total as f64 / (1 << 20) as f64) / write_secs,
        (total as f64 / (1 << 20) as f64) / read_secs
    );
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
