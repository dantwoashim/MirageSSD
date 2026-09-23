#![allow(unsafe_code)]

//! Segment roll-over: writes beyond the test-shrunk segment ceiling must
//! produce one sealed payload per segment with byte-exact content. Runs in
//! its own test binary because MIRAGE_TEST_SEGMENT_MAX is process-wide.

use std::path::{Path, PathBuf};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MirageStatus, mirage_engine_create_managed,
    mirage_engine_destroy, mirage_engine_mark_mounted, mirage_file_close, mirage_flush,
    mirage_lookup, mirage_namespace_create, mirage_read, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

fn build_index() -> (tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), b"seed").expect("seed");
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
    (objects, index)
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn payload_files(journal_dir: &Path) -> Vec<String> {
    std::fs::read_dir(journal_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".payload"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn rollover_above_segment_max_splits_payloads() {
    unsafe { std::env::set_var("MIRAGE_TEST_SEGMENT_MAX", "1048576") }; // 1 MiB
    let (_objects, index) = build_index();
    let state_root = tempfile::tempdir().expect("state root");
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
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    let path16 = utf16("roll.bin");
    assert_eq!(
        unsafe { mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0) },
        MirageStatus::Ok
    );
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    // 3 MiB sequential: three 1 MiB segments.
    let half: Vec<u8> = vec![0xa5; 512 << 10];
    for i in 0..6u64 {
        let mut transferred = 0usize;
        assert_eq!(
            unsafe {
                mirage_write(
                    file,
                    i * 512 * 1024,
                    half.as_ptr(),
                    half.len(),
                    &mut transferred,
                )
            },
            MirageStatus::Ok
        );
    }
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    let journal = state_root.path().join("journal");
    assert_eq!(payload_files(&journal).len(), 3, "3 MiB at 1 MiB/segment");
    let mut buf = vec![0u8; 3 << 20];
    let mut transferred = 0usize;
    assert_eq!(
        unsafe { mirage_read(file, 0, buf.as_mut_ptr(), buf.len(), &mut transferred) },
        MirageStatus::Ok
    );
    buf.truncate(transferred);
    assert_eq!(buf, vec![0xa5u8; 3 << 20]);
    assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
