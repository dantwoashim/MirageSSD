#![allow(unsafe_code)]

//! Unmounted managed-engine gates: startup recovery honesty, the
//! dirty-payload budget ledger, and quiesce-time compaction.

use std::path::{Path, PathBuf};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageStatus, mirage_engine_compact,
    mirage_engine_create_managed, mirage_engine_destroy, mirage_engine_mark_mounted,
    mirage_file_close, mirage_flush, mirage_lookup, mirage_namespace_create,
    mirage_namespace_delete, mirage_read, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

fn build_index() -> (tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), b"seed").expect("seed file");
    std::fs::create_dir_all(source.path().join("nested")).expect("nested dir");
    std::fs::write(source.path().join("nested").join("inner.dat"), b"inner")
        .expect("nested seed file");
    let objects = tempfile::tempdir().expect("objects");
    let imported = import_local(&ImportPlan {
        repository_id: RepositoryId::from_bytes([9; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![
            PlannedFile {
                relative_path: "base.dat".into(),
                class: FileClass::VirtualContainer,
            },
            PlannedFile {
                relative_path: "nested/inner.dat".into(),
                class: FileClass::VirtualContainer,
            },
        ],
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
    assert!(!engine.is_null());
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
fn failed_startup_recovery_never_reports_ready() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    // First owner mounts, then dies without quiescing: the stale Mounted
    // owner record makes the next owner start in Recovering.
    let engine = managed_engine(&index, state_root.path(), 1 << 20);
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    // A pending operation's payload is staged but never committed.
    let volume = RepositoryId::from_bytes([9; 16]);
    let db = mirage_db::Database::open(&state_root.path().join("control.db")).expect("db");
    let journal = mirage_engine::journal::LocalJournal::new(db, volume);
    let journal_dir = state_root.path().join("journal");
    std::fs::create_dir_all(&journal_dir).expect("journal dir");
    let staged = journal
        .stage_payload(&journal_dir, b"pending-bytes")
        .expect("stage");
    let payload_path = journal_dir.join(&staged.path);
    journal
        .begin(
            mirage_db::OperationKind::Write,
            b"pending".to_vec(),
            None,
            None,
            vec![staged],
            1,
        )
        .expect("begin");
    // Replace the payload file with a directory of the same name so the
    // recovery reclaim's file removal must fail.
    std::fs::remove_file(&payload_path).expect("remove payload file");
    std::fs::create_dir(&payload_path).expect("directory at payload path");
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
    // A graceful destroy marks the owner record unmounted; simulate the
    // crashed-owner case by restoring a live state — the next acquire then
    // adopts the record as Recovering.
    let record_path = state_root.path().join(format!(
        "volume-owner-{}.json",
        RepositoryId::from_bytes([9; 16])
    ));
    let record = std::fs::read_to_string(&record_path).expect("owner record");
    std::fs::write(&record_path, record.replace("\"unmounted\"", "\"mounted\""))
        .expect("restore mounted record");

    let engine = managed_engine(&index, state_root.path(), 1 << 20);
    assert_ne!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    let coordinator = unsafe { &*engine }
        .coordinator
        .as_ref()
        .expect("coordinator");
    assert_eq!(
        coordinator.state(),
        mirage_engine::volume::VolumeState::Recovering
    );

    std::fs::remove_dir(&payload_path).expect("cleanup");
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn dirty_budget_bounds_writes_and_survives_restart() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 8192);
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    let file = create_and_open(engine, "data.bin");
    let chunk = vec![0xabu8; 4096];
    assert_eq!(write(file, 0, &chunk), MirageStatus::Ok);
    assert_eq!(write(file, 4096, &chunk), MirageStatus::Ok);
    assert_eq!(write(file, 8192, &[1]), MirageStatus::DiskFull);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);

    // An orphan payload file (no ledger extent, no extent reference) must be
    // swept at engine creation while referenced payloads survive.
    let journal_dir = state_root.path().join("journal");
    // Two contiguous 4 KiB writes coalesce into one sealed segment payload.
    assert_eq!(payload_files(&journal_dir).len(), 1);
    let orphan = journal_dir.join(format!("{}.payload", "00".repeat(16)));
    std::fs::write(&orphan, b"orphan").expect("plant orphan");

    let engine = managed_engine(&index, state_root.path(), 8192);
    assert!(!orphan.exists(), "orphan payload was not swept");
    // Two contiguous 4 KiB writes coalesce into one sealed segment payload.
    assert_eq!(payload_files(&journal_dir).len(), 1);
    // The ledger replay still enforces the budget.
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    let file = open_existing(engine, "data.bin");
    assert_eq!(write(file, 8192, &[1]), MirageStatus::DiskFull);
    assert_eq!(
        read_exact(file, 0, 8192),
        vec![0xabu8; 4096]
            .into_iter()
            .chain(vec![0xabu8; 4096])
            .collect::<Vec<u8>>()
    );
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn compact_reclaims_superseded_payloads() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 1 << 20);
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    let file = create_and_open(engine, "doc.bin");
    assert_eq!(write(file, 0, &[b'A'; 4096]), MirageStatus::Ok);
    assert_eq!(write(file, 0, &[b'B'; 4096]), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    let journal_dir = state_root.path().join("journal");
    assert_eq!(payload_files(&journal_dir).len(), 2);

    assert_eq!(unsafe { mirage_engine_compact(engine) }, MirageStatus::Ok);
    assert_eq!(payload_files(&journal_dir).len(), 1);
    let dirty = unsafe { &*engine }.dirty.as_ref().expect("dirty ledger");
    assert_eq!(dirty.used.load(std::sync::atomic::Ordering::Acquire), 4096);
    assert_eq!(read_exact(file, 0, 4096), vec![b'B'; 4096]);
    // Idempotent.
    assert_eq!(unsafe { mirage_engine_compact(engine) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn nested_paths_resolve_and_deletes_survive_restart() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 1 << 20);
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );

    // Both separators resolve the seeded nested file.
    let backslash = open_existing(engine, "nested\\inner.dat");
    assert_eq!(unsafe { mirage_file_close(backslash) }, MirageStatus::Ok);
    let slash = open_existing(engine, "nested/inner.dat");
    assert_eq!(unsafe { mirage_file_close(slash) }, MirageStatus::Ok);

    // Path components are validated: traversal and embedded NUL refuse.
    let mut traversal = utf16("nested\\..\\base.dat");
    assert_eq!(
        unsafe {
            mirage_lookup(
                engine,
                traversal.as_ptr(),
                traversal.len(),
                &mut std::ptr::null_mut(),
            )
        },
        MirageStatus::InvalidArgument
    );
    traversal.push(0);
    assert_eq!(
        unsafe {
            mirage_lookup(
                engine,
                traversal.as_ptr(),
                traversal.len(),
                &mut std::ptr::null_mut(),
            )
        },
        MirageStatus::InvalidArgument
    );

    // Create + write in a nested directory resolves and opens cleanly.
    let created = create_and_open(engine, "nested\\deep.txt");
    assert_eq!(write(created, 0, b"deep"), MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(created) }, MirageStatus::Ok);

    // Delete a seeded root file and a seeded nested file.
    for doomed in ["base.dat", "nested\\inner.dat"] {
        let path16 = utf16(doomed);
        assert_eq!(
            unsafe { mirage_namespace_delete(engine, path16.as_ptr(), path16.len()) },
            MirageStatus::Ok
        );
    }
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);

    // The seed marker is recorded once; the second engine must not reseed.
    let db = mirage_db::Database::open(&state_root.path().join("control.db")).expect("db");
    let marker = db
        .namespace_seed_marker(RepositoryId::from_bytes([9; 16]))
        .expect("marker read");
    let index_hash = mirage_index::MountIndex::open(&index)
        .expect("index")
        .header()
        .index_hash;
    assert_eq!(marker, Some(index_hash));
    drop(db);

    let engine = managed_engine(&index, state_root.path(), 1 << 20);
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    for gone in ["base.dat", "nested\\inner.dat", "nested/inner.dat"] {
        let path16 = utf16(gone);
        let mut file: *mut MirageFileHandle = std::ptr::null_mut();
        assert_eq!(
            unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
            MirageStatus::NotFound,
            "deleted seeded entry resurrected: {gone}"
        );
    }
    // The locally created nested file survived with its bytes.
    let file = open_existing(engine, "nested\\deep.txt");
    assert_eq!(read_exact(file, 0, 4), b"deep");
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
