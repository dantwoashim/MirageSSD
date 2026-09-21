#![allow(unsafe_code)]

//! Whole-file prefetch: the first provider miss of a <=64 MiB file schedules
//! every non-resident page on the speculative queue, so a cold small file is
//! fully resident after one foreground fetch.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageStatus,
    mirage_engine_create_managed_local_provider, mirage_engine_destroy, mirage_engine_mark_mounted,
    mirage_engine_set_drive_token, mirage_lookup, mirage_read,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const PAGES: usize = 5;
const PAGE: usize = 64 * 1024;

fn build_index() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    // Distinct pages so every page hash differs.
    let mut content = Vec::with_capacity(PAGES * PAGE);
    for page in 0..PAGES {
        content.extend(std::iter::repeat_n(page as u8 + 1, PAGE));
    }
    std::fs::write(source.path().join("big.bin"), &content).expect("seed");
    let objects = tempfile::tempdir().expect("objects");
    let imported = import_local(&ImportPlan {
        repository_id: RepositoryId::from_bytes([9; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![PlannedFile {
            relative_path: "big.bin".into(),
            class: FileClass::VirtualContainer,
        }],
        page_size: PAGE as u32,
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
        page_size: mirage_types::ByteCount::from_u64(PAGE as u64),
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

fn resident_pages(state_root: &Path) -> usize {
    let db = mirage_db::Database::open(&state_root.join("control.db")).expect("db");
    db.load_resident_cache_slots()
        .expect("resident slots")
        .iter()
        .filter(|slot| slot.page_hash.is_some())
        .count()
}

#[test]
fn first_miss_prefetches_the_whole_small_file() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let index16: Vec<u16> = index.to_string_lossy().encode_utf16().collect();
    let root16: Vec<u16> = state.path().to_string_lossy().encode_utf16().collect();
    let provider16: Vec<u16> = objects.path().to_string_lossy().encode_utf16().collect();
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
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    assert_eq!(
        unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
        MirageStatus::Ok
    );
    assert_eq!(resident_pages(state.path()), 0);

    // One 4 KiB read: the demand fetch lands page 1, the whole-file prefetch
    // schedules pages 2-5 on the speculative queue.
    let path16: Vec<u16> = "big.bin".encode_utf16().collect();
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    let mut output = vec![0u8; 4096];
    let mut transferred = 0usize;
    assert_eq!(
        unsafe { mirage_read(file, 0, output.as_mut_ptr(), output.len(), &mut transferred) },
        MirageStatus::Ok
    );
    assert_eq!(transferred, 4096);
    assert!(output.iter().all(|b| *b == 1));

    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && resident_pages(state.path()) < PAGES {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        resident_pages(state.path()),
        PAGES,
        "whole-file prefetch did not make every page resident"
    );
    unsafe { mirage_ffi::mirage_file_close(file) };
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
