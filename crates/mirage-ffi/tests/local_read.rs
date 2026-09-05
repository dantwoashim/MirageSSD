#![allow(unsafe_code)]

use mirage_cache::{ArenaShard, CacheLayout, insert_page};
use mirage_db::{CacheShardSpec, Database};
use mirage_ffi::{MirageEngineHandle, MirageFileHandle, MirageStatus};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PackEncryption, PlannedFile, import_local};
use mirage_types::{ByteCount, GenerationId, RepositoryId};
use std::sync::Arc;

fn utf16(path: &std::path::Path) -> Vec<u16> {
    path.as_os_str().to_string_lossy().encode_utf16().collect()
}

#[test]
fn index_namespace_and_local_pack_reads_are_exact() {
    run_local_read(false);
}

#[cfg(windows)]
#[test]
fn encrypted_local_pack_reads_are_exact() {
    run_local_read(true);
}

fn run_local_read(encrypted: bool) {
    let source = tempfile::tempdir().expect("source");
    std::fs::create_dir(source.path().join("assets")).expect("directory");
    let mut expected = vec![0x31; 64 * 1024];
    expected.extend((0..31_337).map(|value| (value % 251) as u8));
    std::fs::write(source.path().join("assets/data.pak"), &expected).expect("source file");
    let objects = tempfile::tempdir().expect("objects");
    let repository_id = RepositoryId::from_bytes([9; 16]);
    let key = encrypted.then(|| Arc::new(mirage_crypto::aead::RepositoryKey::generate().unwrap()));
    let imported = import_local(&ImportPlan {
        repository_id,
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![PlannedFile {
            relative_path: "assets/data.pak".into(),
            class: FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 256 * 1024,
        output_staging_directory: objects.path().to_path_buf(),
        encryption: key.as_ref().map(|key| PackEncryption {
            repository_id,
            key: Arc::clone(key),
        }),
    })
    .expect("import");
    #[cfg(windows)]
    if let Some(key) = &key {
        mirage_crypto::repository_key_store::save_repository_key(
            &objects.path().join("repository-key.dpapi"),
            repository_id,
            key,
            mirage_crypto::dpapi::ProtectionScope::LocalMachine,
        )
        .expect("save repository key");
    }
    let index_path = objects.path().join("mount.idx");
    mirage_index::compile_to_path(&imported.manifest, &index_path).expect("index");
    let index = utf16(&index_path);
    let root = utf16(objects.path());
    let mut engine: *mut MirageEngineHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            mirage_ffi::mirage_engine_create_local(
                index.as_ptr(),
                index.len(),
                root.as_ptr(),
                root.len(),
                &mut engine,
            )
        },
        MirageStatus::Ok
    );
    let path: Vec<u16> = "\\assets\\data.pak".encode_utf16().collect();
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_ffi::mirage_lookup(engine, path.as_ptr(), path.len(), &mut file) },
        MirageStatus::Ok
    );
    let mut actual = vec![0; expected.len()];
    let mut transferred = 0;
    assert_eq!(
        unsafe {
            mirage_ffi::mirage_read(file, 0, actual.as_mut_ptr(), actual.len(), &mut transferred)
        },
        MirageStatus::Ok
    );
    assert_eq!(transferred, expected.len());
    assert_eq!(actual, expected);
    unsafe {
        mirage_ffi::mirage_file_close(file);
        mirage_ffi::mirage_engine_destroy(engine);
    }
}

#[test]
fn verified_sparse_cache_reads_are_exact_without_pack_access() {
    let source = tempfile::tempdir().expect("source");
    std::fs::create_dir(source.path().join("assets")).expect("directory");
    let mut expected = vec![0x42; 64 * 1024];
    expected.extend((0..19_871).map(|value| (value % 239) as u8));
    std::fs::write(source.path().join("assets/data.pak"), &expected).expect("source file");
    let objects = tempfile::tempdir().expect("objects");
    let imported = import_local(&ImportPlan {
        repository_id: RepositoryId::from_bytes([0x44; 16]),
        generation_id: GenerationId::ZERO,
        source_root: source.path().to_path_buf(),
        files: vec![PlannedFile {
            relative_path: "assets/data.pak".into(),
            class: FileClass::VirtualContainer,
        }],
        page_size: 64 * 1024,
        pack_target: 256 * 1024,
        output_staging_directory: objects.path().to_path_buf(),
        encryption: None,
    })
    .expect("import");
    let index_path = objects.path().join("cache.idx");
    mirage_index::compile_to_path(&imported.manifest, &index_path).expect("index");

    let state = tempfile::tempdir().expect("state");
    std::fs::create_dir(state.path().join("cache")).expect("cache directory");
    let database = Database::open(&state.path().join("control.db")).expect("database");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(64 * 1024),
        slot_count: imported.manifest.pages.len() as u32,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let shard = Arc::new(
        ArenaShard::create(&state.path().join("cache/shard-0.bin"), layout).expect("shard"),
    );
    database
        .register_cache_shard(CacheShardSpec {
            shard_id: 0,
            relative_path: "shard-0.bin".into(),
            page_size: layout.page_size,
            slot_count: layout.slot_count,
        })
        .expect("register shard");
    for (chunk, page) in expected.chunks(64 * 1024).zip(&imported.manifest.pages) {
        insert_page(
            &database,
            Arc::clone(&shard),
            page.plaintext_hash,
            chunk,
            &(),
        )
        .expect("insert cache page");
    }
    drop(shard);
    drop(database);

    let index = utf16(&index_path);
    let root = utf16(state.path());
    let mut engine: *mut MirageEngineHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe {
            mirage_ffi::mirage_engine_create_cache(
                index.as_ptr(),
                index.len(),
                root.as_ptr(),
                root.len(),
                &mut engine,
            )
        },
        MirageStatus::Ok
    );
    let path: Vec<u16> = "\\assets\\data.pak".encode_utf16().collect();
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_ffi::mirage_lookup(engine, path.as_ptr(), path.len(), &mut file) },
        MirageStatus::Ok
    );
    let mut actual = vec![0; expected.len()];
    let mut transferred = 0;
    assert_eq!(
        unsafe {
            mirage_ffi::mirage_read(file, 0, actual.as_mut_ptr(), actual.len(), &mut transferred)
        },
        MirageStatus::Ok
    );
    assert_eq!(transferred, expected.len());
    assert_eq!(actual, expected);
    unsafe {
        mirage_ffi::mirage_file_close(file);
        mirage_ffi::mirage_engine_destroy(engine);
    }
}
