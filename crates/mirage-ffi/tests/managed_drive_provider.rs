#![allow(unsafe_code)]

//! Managed-engine on-demand provider gates: a local pack mirror stands in for
//! the Drive backend — the provider installs only on the first token push,
//! fetched pages are verified, admitted, and served from residency on repeat
//! reads.

use std::path::{Path, PathBuf};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageStatus,
    mirage_engine_create_managed_local_provider, mirage_engine_destroy, mirage_engine_mark_mounted,
    mirage_engine_set_drive_token, mirage_lookup, mirage_read,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const BUDGET: u64 = 64 * 1024 * 1024;

fn build_index() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), vec![0x5Au8; 100 * 1024]).expect("seed file");
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
    (source, objects, index)
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// A state root with an empty cache shard registered in control.db so the
/// managed engine has resident/shard state for the provider to admit into.
fn provisioned_state(page_size: u64, slots: u32) -> tempfile::TempDir {
    let state = tempfile::tempdir().expect("state");
    std::fs::create_dir(state.path().join("cache")).expect("cache dir");
    let database = mirage_db::Database::open(&state.path().join("control.db")).expect("database");
    let layout = mirage_cache::CacheLayout {
        page_size: mirage_types::ByteCount::from_u64(page_size),
        slot_count: slots,
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
            slot_count: slots,
        })
        .expect("register shard");
    state
}

fn managed_engine(
    index: &Path,
    state_root: &Path,
    provider_root: &Path,
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
                BUDGET,
                provider16.as_ptr(),
                provider16.len(),
                &mut engine,
            )
        },
        MirageStatus::Ok
    );
    assert!(!engine.is_null());
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

fn read(file: *mut MirageFileHandle, length: usize) -> (MirageStatus, Vec<u8>) {
    let mut output = vec![0u8; length];
    let mut transferred = 0usize;
    let status = unsafe { mirage_read(file, 0, output.as_mut_ptr(), length, &mut transferred) };
    output.truncate(transferred);
    (status, output)
}

#[test]
fn provider_fetch_installs_on_first_token_and_serves_resident() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    let engine = managed_engine(&index, state.path(), objects.path());
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    let file = open(engine, "base.dat");

    // No credential yet: the provider is not installed and no fetch is
    // attempted — the read fails with the unavailable status.
    let (status, _) = read(file, 100 * 1024);
    assert_eq!(status, MirageStatus::BackendUnavailable);

    assert_eq!(
        unsafe {
            mirage_engine_set_drive_token(engine, b"test-token".as_ptr(), b"test-token".len())
        },
        MirageStatus::Ok
    );
    let (status, bytes) = read(file, 100 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, vec![0x5Au8; 100 * 1024]);

    // The fetched page was admitted: removing the pack mirror still serves
    // the page from residency.
    for entry in std::fs::read_dir(objects.path()).expect("objects") {
        let entry = entry.expect("entry");
        if entry.file_name() != "mount.idx" {
            std::fs::remove_file(entry.path()).expect("remove pack");
        }
    }
    let (status, bytes) = read(file, 100 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, vec![0x5Au8; 100 * 1024]);

    unsafe { mirage_ffi::mirage_file_close(file) };
    unsafe { mirage_engine_destroy(engine) };
}

#[test]
fn corrupt_provider_object_fails_closed() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    let engine = managed_engine(&index, state.path(), objects.path());
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    assert_eq!(
        unsafe {
            mirage_engine_set_drive_token(engine, b"test-token".as_ptr(), b"test-token".len())
        },
        MirageStatus::Ok
    );

    // Corrupt bytes inside the pack that backs base.dat.
    let pack = std::fs::read_dir(objects.path())
        .expect("objects")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pack-"))
        })
        .expect("pack object");
    let mut bytes = std::fs::read(&pack).expect("pack bytes");
    for byte in &mut bytes {
        *byte ^= 0xFF;
    }
    std::fs::write(&pack, &bytes).expect("corrupt pack");

    let file = open(engine, "base.dat");
    let (status, _) = read(file, 100 * 1024);
    assert_eq!(status, MirageStatus::IntegrityFailure);
    // Nothing was placed: a second attempt fails identically.
    let (status, _) = read(file, 100 * 1024);
    assert_eq!(status, MirageStatus::IntegrityFailure);

    unsafe { mirage_ffi::mirage_file_close(file) };
    unsafe { mirage_engine_destroy(engine) };
}
