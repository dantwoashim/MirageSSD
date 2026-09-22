#![allow(unsafe_code)]

//! A managed volume built on an explicitly empty index: zero files, zero
//! packs — the shape `mirage volume create` produces for a brand-new Drive
//! folder. Mount it, create a file, read it back, enumerate the root.

use std::path::{Path, PathBuf};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageFileInfo, MirageStatus,
    mirage_engine_create_managed_local_provider, mirage_engine_destroy, mirage_engine_mark_mounted,
    mirage_engine_set_drive_token, mirage_enumerate, mirage_flush, mirage_lookup,
    mirage_namespace_create, mirage_read, mirage_write,
};
use mirage_pack::{ImportPlan, import_local_allow_empty};
use mirage_types::{GenerationId, RepositoryId};

fn empty_index() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    let objects = tempfile::tempdir().expect("objects");
    let imported = import_local_allow_empty(&ImportPlan {
        repository_id: RepositoryId::from_bytes([9; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: Vec::new(),
        page_size: 64 * 1024,
        pack_target: 2 * 1024 * 1024,
        output_staging_directory: objects.path().to_path_buf(),
        encryption: None,
    })
    .expect("empty import");
    assert_eq!(imported.report.pack_count, 0);
    assert_eq!(imported.report.logical_bytes, 0);
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

fn managed_local(index: &Path, state_root: &Path, provider_root: &Path) -> *mut MirageEngineHandle {
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
                64 * 1024 * 1024,
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

fn children(root: *mut MirageFileHandle) -> Vec<String> {
    unsafe extern "C" fn collect(
        context: *mut core::ffi::c_void,
        name: *const u16,
        name_len: usize,
        _info: MirageFileInfo,
    ) -> u8 {
        let names = unsafe { &mut *(context as *mut Vec<String>) };
        names.push(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(name, name_len)
        }));
        1
    }
    let mut names = Vec::new();
    assert_eq!(
        unsafe {
            mirage_enumerate(
                root,
                std::ptr::null(),
                0,
                4096,
                &mut names as *mut Vec<String> as *mut core::ffi::c_void,
                Some(collect),
            )
        },
        MirageStatus::Ok
    );
    names
}

#[test]
fn empty_managed_volume_accepts_writes_and_lists_the_root() {
    let (_source, objects, index) = empty_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());

    // The empty namespace resolves the root and lists no children.
    let mut root: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, std::ptr::null(), 0, &mut root) },
        MirageStatus::Ok
    );
    assert!(children(root).is_empty());

    // First write: create a file, write bytes, flush, read back exactly.
    let path16 = utf16("hello.txt");
    assert_eq!(
        unsafe { mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0) },
        MirageStatus::Ok
    );
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    let payload = b"fresh volume, first bytes";
    let mut transferred = 0usize;
    assert_eq!(
        unsafe { mirage_write(file, 0, payload.as_ptr(), payload.len(), &mut transferred,) },
        MirageStatus::Ok
    );
    assert_eq!(transferred, payload.len());
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);

    let mut read_back = vec![0_u8; payload.len()];
    let mut read = 0usize;
    assert_eq!(
        unsafe { mirage_read(file, 0, read_back.as_mut_ptr(), read_back.len(), &mut read) },
        MirageStatus::Ok
    );
    assert_eq!(read, payload.len());
    assert_eq!(read_back, payload);

    // The durable namespace now lists the file under the root.
    assert_eq!(children(root), vec!["hello.txt".to_owned()]);

    unsafe { mirage_ffi::mirage_file_close(file) };
    unsafe { mirage_ffi::mirage_file_close(root) };
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
