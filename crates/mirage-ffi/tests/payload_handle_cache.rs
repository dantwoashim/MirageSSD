#![allow(unsafe_code)]

//! Shared payload read-handle cache: dirty-slice reads reuse open `File`s
//! instead of a CreateFile per slice, and deleting a payload must drop its
//! cached handle first so the name is not left delete-pending.

use std::path::{Path, PathBuf};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageStatus, mirage_engine_create_managed,
    mirage_engine_destroy, mirage_engine_mark_mounted, mirage_file_close, mirage_lookup,
    mirage_namespace_create, mirage_namespace_delete, mirage_read, mirage_write,
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

/// Reads of an open segment go through the shared payload handle the writer
/// appends to — interleaved writes and reads must each see the right bytes
/// (cursor safety on the shared handle).
#[test]
fn reads_on_an_open_segment_interleave_with_appends() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let file = create_and_open(engine, "\\live.bin");
    // No flush/close: the segment stays open while reads hit its payload.
    let mut expected: Vec<u8> = Vec::new();
    let mut offset = 0u64;
    for round in 0..8u32 {
        let chunk: Vec<u8> = (0..4096u32)
            .map(|i| ((i + round * 4096) % 251) as u8)
            .collect();
        assert_eq!(write(file, offset, &chunk), MirageStatus::Ok);
        expected.extend_from_slice(&chunk);
        offset += chunk.len() as u64;
        // Read the whole file back while the segment is still open.
        assert_eq!(read_exact(file, 0, expected.len()), expected);
        // And a middle slice that crosses earlier appends.
        if expected.len() >= 8192 {
            assert_eq!(read_exact(file, 100, 4096), expected[100..4196]);
        }
    }
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

/// With the cache cap forced to 1, every second payload insert evicts the
/// previous entry — reads must fall back to a fresh open and still return
/// the right bytes.
#[test]
fn lru_evicted_payloads_still_read_correctly() {
    unsafe { std::env::set_var("MIRAGE_PAYLOAD_CACHE_LIMIT", "1") };
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let payload_a: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let payload_b: Vec<u8> = (0..4096u32).map(|i| ((i + 7) % 251) as u8).collect();
    let a = create_and_open(engine, "\\lru-a.bin");
    let b = create_and_open(engine, "\\lru-b.bin");
    assert_eq!(write(a, 0, &payload_a), MirageStatus::Ok);
    assert_eq!(write(b, 0, &payload_b), MirageStatus::Ok);
    // Insert order is A then B; B's insert evicted A's entry.
    assert_eq!(read_exact(a, 0, payload_a.len()), payload_a);
    assert_eq!(read_exact(b, 0, payload_b.len()), payload_b);
    // Reading A again re-evicts B — churn must not corrupt either file.
    assert_eq!(read_exact(a, 0, payload_a.len()), payload_a);
    assert_eq!(read_exact(b, 0, payload_b.len()), payload_b);
    assert_eq!(unsafe { mirage_file_close(a) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(b) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
    unsafe { std::env::remove_var("MIRAGE_PAYLOAD_CACHE_LIMIT") };
}

#[test]
fn discarded_payload_leaves_no_cached_handle_pinning_the_name() {
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
    let engine = managed_engine(&index, state_root.path(), 64 << 20);
    let journal_dir = state_root.path().join("journal");
    let a = create_and_open(engine, "\\a.bin");
    let b = create_and_open(engine, "\\b.bin");
    let payload_a: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
    let payload_b: Vec<u8> = (0..4096u32).map(|i| ((i + 7) % 251) as u8).collect();
    assert_eq!(write(a, 0, &payload_a), MirageStatus::Ok);
    assert_eq!(write(b, 0, &payload_b), MirageStatus::Ok);
    // Reads go through the shared cache — both payload handles are now open.
    assert_eq!(read_exact(a, 0, payload_a.len()), payload_a);
    assert_eq!(read_exact(b, 0, payload_b.len()), payload_b);
    let before = payload_files(&journal_dir);
    assert_eq!(before.len(), 2, "two open-segment payloads on disk");

    // Deleting A discards its open segment: the payload file is removed and
    // its cached read handle must be dropped first.
    let del = utf16("\\a.bin");
    assert_eq!(
        unsafe { mirage_namespace_delete(engine, del.as_ptr(), del.len()) },
        MirageStatus::Ok
    );
    let after = payload_files(&journal_dir);
    assert_eq!(after.len(), 1, "deleted file's payload must be gone");
    let removed = before
        .iter()
        .find(|name| !after.contains(name))
        .expect("removed payload name");

    // A cached handle would leave the name delete-pending: create_new would
    // fail while any handle is open. Successful recreation proves the cache
    // dropped the handle before the delete ran.
    let recreated = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(journal_dir.join(removed));
    assert!(
        recreated.is_ok(),
        "stale cached handle kept the deleted payload name pinned"
    );
    drop(recreated);
    let _ = std::fs::remove_file(journal_dir.join(removed));

    // The surviving file still reads the right bytes through the cache.
    assert_eq!(read_exact(b, 0, payload_b.len()), payload_b);
    assert_eq!(unsafe { mirage_file_close(a) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_file_close(b) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
