#![allow(unsafe_code)]

//! Cloud-drain probe: how many remote objects (and therefore Drive write
//! requests) N small files cost, and how long publication takes through the
//! local directory backend. Google Drive caps sustained write/insert requests
//! at about 3 per second per account (support.google.com/a/answer/10445916),
//! and a resumable upload costs at least 2 of them (session start + upload),
//! so the modeled Drive floor is objects x 2 / 3 seconds.
//! Ignored by default; run with
//! `cargo test --release -p mirage-ffi --test publish_drain_probe -- --ignored --nocapture`
//! (`PROBE_FILES` / `PROBE_BYTES` override the defaults).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MiragePublicationStats, MirageStatus,
    mirage_engine_create_managed_local_provider, mirage_engine_destroy, mirage_engine_mark_mounted,
    mirage_engine_publication_stats, mirage_engine_set_drive_token, mirage_file_close,
    mirage_lookup, mirage_namespace_create, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn build_index() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), vec![0x5Au8; 100 * 1024]).expect("seed");
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
    (source, objects, index)
}

fn provisioned_state(page_size: u64, slots: u32) -> tempfile::TempDir {
    let state = tempfile::tempdir().expect("state");
    std::fs::create_dir(state.path().join("cache")).expect("cache dir");
    let database = mirage_db::Database::open(&state.path().join("control.db")).expect("db");
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

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn engine(index: &Path, state_root: &Path, provider_root: &Path) -> *mut MirageEngineHandle {
    let index16 = utf16(&index.to_string_lossy());
    let root16 = utf16(&state_root.to_string_lossy());
    let provider16 = utf16(&provider_root.to_string_lossy());
    let mut engine: *mut MirageEngineHandle = std::ptr::null_mut();
    let status = unsafe {
        mirage_engine_create_managed_local_provider(
            index16.as_ptr(),
            index16.len(),
            root16.as_ptr(),
            root16.len(),
            std::ptr::null(),
            0,
            4 << 30,
            provider16.as_ptr(),
            provider16.len(),
            &mut engine,
        )
    };
    assert_eq!(status, MirageStatus::Ok);
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

fn stats(engine: *mut MirageEngineHandle) -> MiragePublicationStats {
    let mut stats = MiragePublicationStats {
        pending_payloads: 0,
        pending_bytes: 0,
        published_payloads: 0,
        published_bytes: 0,
        evicted_payloads: 0,
        integrity_refusals: 0,
        last_error_class: [0; 32],
    };
    assert_eq!(
        unsafe { mirage_engine_publication_stats(engine, &mut stats) },
        MirageStatus::Ok
    );
    stats
}

#[test]
#[ignore = "timing probe; run explicitly with --release --ignored --nocapture"]
fn publish_drain_probe() {
    let files = env_usize("PROBE_FILES", 500);
    let bytes = env_usize("PROBE_BYTES", 4096);
    let (_source, _objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 64);
    let provider = tempfile::tempdir().expect("provider root");
    let engine = engine(&index, state.path(), provider.path());

    let started = Instant::now();
    for i in 0..files {
        let path = format!("\\f{i:05}.bin");
        let path16 = utf16(&path);
        assert_eq!(
            unsafe { mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0) },
            MirageStatus::Ok
        );
        let mut file: *mut MirageFileHandle = std::ptr::null_mut();
        assert_eq!(
            unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
            MirageStatus::Ok
        );
        let data: Vec<u8> = (0..bytes).map(|j| ((i * 131 + j) % 251) as u8).collect();
        let mut transferred = 0usize;
        assert_eq!(
            unsafe { mirage_write(file, 0, data.as_ptr(), data.len(), &mut transferred) },
            MirageStatus::Ok
        );
        assert_eq!(unsafe { mirage_file_close(file) }, MirageStatus::Ok);
    }
    let written = started.elapsed();

    let deadline = Instant::now() + Duration::from_secs(900);
    let mut last = stats(engine);
    while (last.published_payloads as usize) < files && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        last = stats(engine);
    }
    let drained = started.elapsed();
    let objects = std::fs::read_dir(provider.path().join("payloads"))
        .map(|entries| entries.flatten().count())
        .unwrap_or(0);
    let object_bytes: u64 = std::fs::read_dir(provider.path().join("payloads"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.metadata().ok())
                .map(|meta| meta.len())
                .sum()
        })
        .unwrap_or(0);
    let error = String::from_utf8_lossy(&last.last_error_class)
        .trim_end_matches('\0')
        .to_string();

    println!("=== publish drain probe: {files} files x {bytes} B ===");
    println!(
        "local writes finished           {:>9.2} s",
        written.as_secs_f64()
    );
    println!(
        "all payloads published          {:>9.2} s (local directory backend)",
        drained.as_secs_f64()
    );
    println!(
        "published payloads              {:>9}",
        last.published_payloads
    );
    println!("remote objects created          {:>9}", objects);
    println!("remote bytes                    {:>9}", object_bytes);
    println!(
        "modeled Drive write-cap floor   {:>9.1} s (objects x 2 requests / 3 per s)",
        objects as f64 * 2.0 / 3.0
    );
    println!("last publish error class        {error:?}");
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}
