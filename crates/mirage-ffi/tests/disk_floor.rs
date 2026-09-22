#![allow(unsafe_code)]

//! Disk-floor admission and explicit eviction (`mirage_engine_evict_published`,
//! `mirage_engine_set_disk_floor`) through the directory-backend seam.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MiragePublicationStats, MirageStatus,
    mirage_engine_create_managed_local_provider, mirage_engine_destroy,
    mirage_engine_evict_published, mirage_engine_mark_mounted, mirage_engine_publication_stats,
    mirage_engine_set_disk_floor, mirage_engine_set_drive_token, mirage_flush, mirage_lookup,
    mirage_namespace_create, mirage_read, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const BUDGET: u64 = 64 * 1024 * 1024;

fn build_index() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), vec![0x5Au8; 64 * 1024]).expect("seed");
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
    managed_with_budget(index, state_root, provider_root, BUDGET)
}

fn managed_with_budget(
    index: &Path,
    state_root: &Path,
    provider_root: &Path,
    budget: u64,
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
                budget,
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

fn create_write(engine: *mut MirageEngineHandle, path: &str, bytes: &[u8]) -> MirageStatus {
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
    let mut transferred = 0usize;
    let status = unsafe { mirage_write(file, 0, bytes.as_ptr(), bytes.len(), &mut transferred) };
    if status == MirageStatus::Ok {
        assert_eq!(transferred, bytes.len());
        assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    }
    unsafe { mirage_ffi::mirage_file_close(file) };
    status
}

fn stats(engine: *mut MirageEngineHandle) -> MiragePublicationStats {
    let mut output = MiragePublicationStats {
        pending_payloads: 0,
        pending_bytes: 0,
        published_payloads: 0,
        published_bytes: 0,
        evicted_payloads: 0,
        integrity_refusals: 0,
        last_error_class: [0; 32],
    };
    assert_eq!(
        unsafe { mirage_engine_publication_stats(engine, &mut output) },
        MirageStatus::Ok
    );
    output
}

fn wait_for(engine: *mut MirageEngineHandle, predicate: impl Fn(&MiragePublicationStats) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if predicate(&stats(engine)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("stats never satisfied predicate: {:?}", stats(engine));
}

fn read_path(engine: *mut MirageEngineHandle, path: &str, len: usize) -> (MirageStatus, Vec<u8>) {
    let path16 = utf16(path);
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    let mut output = vec![0u8; len];
    let mut transferred = 0usize;
    let status = unsafe { mirage_read(file, 0, output.as_mut_ptr(), len, &mut transferred) };
    output.truncate(transferred);
    unsafe { mirage_ffi::mirage_file_close(file) };
    (status, output)
}

fn payload_names(journal_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(journal_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".payload"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn evict(engine: *mut MirageEngineHandle, target: u64) -> (MirageStatus, u64) {
    let mut freed = 0u64;
    let mut blocked = 0u64;
    let status = unsafe { mirage_engine_evict_published(engine, target, &mut freed, &mut blocked) };
    (status, freed)
}

fn real_free_bytes(dir: &Path) -> u64 {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = dir
            .canonicalize()
            .unwrap()
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut free = 0u64;
        let mut total = 0u64;
        let mut total_free = 0u64;
        assert_ne!(
            unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut total_free) },
            0
        );
        free
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        u64::MAX
    }
}

#[test]
fn evict_published_frees_oldest_only_and_reads_stay_exact() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    let journal = state.path().join("journal");

    create_write(engine, "a.bin", &vec![0xAA; 512 * 1024]);
    create_write(engine, "b.bin", &vec![0xBB; 512 * 1024]);
    create_write(engine, "c.bin", &vec![0xCC; 512 * 1024]);
    wait_for(engine, |s| s.published_payloads >= 3);
    let before = payload_names(&journal);
    assert_eq!(before.len(), 3);

    // A minimal target frees exactly the oldest published payload.
    let (status, freed) = evict(engine, 1);
    assert_eq!(status, MirageStatus::Ok);
    assert!(freed >= 512 * 1024, "freed={freed}");
    let after = payload_names(&journal);
    assert_eq!(after.len(), 2);
    assert_eq!(stats(engine).evicted_payloads, 1);

    let (status, bytes) = read_path(engine, "a.bin", 512 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0xAA));
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn open_handle_protects_its_payload() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    let journal = state.path().join("journal");

    create_write(engine, "held.bin", &vec![0x11; 256 * 1024]);
    create_write(engine, "free.bin", &vec![0x22; 256 * 1024]);
    wait_for(engine, |s| s.published_payloads >= 2);

    // An open handle on held.bin protects its payload from eviction.
    let path16 = utf16("held.bin");
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    let (_status, freed) = evict(engine, u64::MAX);
    assert!(freed >= 256 * 1024, "freed={freed}");
    let names = payload_names(&journal);
    // Exactly one payload survives: the one protected by the open handle.
    assert_eq!(names.len(), 1, "remaining={names:?}");
    let (status, bytes) = read_path(engine, "held.bin", 256 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0x11));
    unsafe { mirage_ffi::mirage_file_close(file) };
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn unpublished_payloads_are_never_evicted() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    let journal = state.path().join("journal");

    create_write(engine, "unpublished.bin", &vec![0x33; 256 * 1024]);
    // Give the publisher a moment, then evict: pending or published it must
    // only ever drop *published* payloads — unpublished never leaves disk.
    let (status, _freed) = evict(engine, u64::MAX);
    assert_eq!(status, MirageStatus::Ok);
    let s = stats(engine);
    // Whatever the publisher already published is legitimately evictable; the
    // file for a still-unpublished payload must remain.
    if s.published_payloads == 0 {
        assert_eq!(payload_names(&journal).len(), 1);
    }
    let (status, bytes) = read_path(engine, "unpublished.bin", 256 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0x33));
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn impossible_floor_fails_diskfull_when_nothing_evictable() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    let journal = state.path().join("journal");
    std::fs::create_dir_all(&journal).expect("journal dir");

    // Floor higher than real free space: nothing published yet → DiskFull.
    let free = real_free_bytes(&journal);
    assert_eq!(
        unsafe { mirage_engine_set_disk_floor(engine, free + 1) },
        MirageStatus::Ok
    );
    assert_eq!(
        create_write(engine, "blocked.bin", &vec![0x44; 4096]),
        MirageStatus::DiskFull
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn breached_floor_evicts_published_then_admits() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    let journal = state.path().join("journal");
    std::fs::create_dir_all(&journal).expect("journal dir");

    // Publish a 2 MiB payload while the floor is off.
    create_write(engine, "published.bin", &vec![0x55; 2 * 1024 * 1024]);
    wait_for(engine, |s| s.published_payloads >= 1);

    // Floor = current free + 1 MiB: a write is over the floor, but evicting
    // the 2 MiB published payload clears the deficit. Free space on the test
    // volume moves under parallel load, so retry with a fresh measurement.
    let mut admitted = false;
    for attempt in 0..16 {
        let free = real_free_bytes(&journal);
        assert_eq!(
            unsafe { mirage_engine_set_disk_floor(engine, free + (1 << 20)) },
            MirageStatus::Ok
        );
        let name = format!("admitted{attempt}.bin");
        match create_write(engine, &name, &vec![0x66; 64 * 1024]) {
            MirageStatus::Ok if stats(engine).evicted_payloads >= 1 => {
                admitted = true;
                break;
            }
            // Free space drifted between measurement and write: retry.
            MirageStatus::Ok | MirageStatus::DiskFull => continue,
            status => panic!("unexpected write status {status:?}"),
        }
    }
    assert!(admitted, "floor admission never succeeded in 16 attempts");
    assert!(stats(engine).evicted_payloads >= 1);
    let (status, bytes) = read_path(engine, "published.bin", 2 * 1024 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0x55));
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

fn control_db(state_root: &Path) -> mirage_db::Database {
    mirage_db::Database::open(&state_root.join("control.db")).expect("control db")
}

fn volume_id() -> RepositoryId {
    RepositoryId::from_bytes([9; 16])
}

#[test]
fn pinned_directory_protects_its_payloads_until_unpinned() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    let journal = state.path().join("journal");

    // keep/ directory gets a pinned payload; free.bin does not.
    let dir16 = utf16("keep");
    assert_eq!(
        unsafe { mirage_namespace_create(engine, dir16.as_ptr(), dir16.len(), 1) },
        MirageStatus::Ok
    );
    create_write(engine, "keep/a.bin", &vec![0x77; 256 * 1024]);
    create_write(engine, "free.bin", &vec![0x88; 256 * 1024]);
    wait_for(engine, |s| s.published_payloads >= 2);

    // Pin the directory through the durable pin table, then refresh the
    // host-side set the way PINS-RELOAD does.
    let db = control_db(state.path());
    let keep_inode = db
        .namespace_resolve_components(volume_id(), &["keep".to_owned()])
        .unwrap()
        .expect("keep inode");
    db.namespace_pin(volume_id(), keep_inode, 1).unwrap();
    assert_eq!(
        unsafe { mirage_ffi::mirage_engine_reload_pins(engine) },
        MirageStatus::Ok
    );

    let mut freed = 0u64;
    let mut blocked = 0u64;
    assert_eq!(
        unsafe { mirage_engine_evict_published(engine, u64::MAX, &mut freed, &mut blocked) },
        MirageStatus::Ok
    );
    assert!(freed >= 256 * 1024, "freed={freed}");
    assert!(blocked >= 256 * 1024, "blocked={blocked}");
    // Exactly the pinned payload survives on disk.
    assert_eq!(payload_names(&journal).len(), 1);
    let (status, bytes) = read_path(engine, "keep/a.bin", 256 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0x77));

    // Unpin → the payload becomes evictable again.
    assert!(db.namespace_unpin(volume_id(), keep_inode).unwrap());
    assert_eq!(
        unsafe { mirage_ffi::mirage_engine_reload_pins(engine) },
        MirageStatus::Ok
    );
    let (status, freed) = evict(engine, u64::MAX);
    assert_eq!(status, MirageStatus::Ok);
    assert!(freed >= 256 * 1024);
    assert!(payload_names(&journal).is_empty());
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn evicted_payload_restages_on_read_within_budget() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    let journal = state.path().join("journal");

    create_write(engine, "restage.bin", &vec![0x99; 256 * 1024]);
    wait_for(engine, |s| s.published_payloads >= 1);
    let (status, freed) = evict(engine, u64::MAX);
    assert_eq!(status, MirageStatus::Ok);
    assert!(freed >= 256 * 1024);
    assert!(payload_names(&journal).is_empty());

    // First read re-stages the whole payload; bytes stay exact.
    let (status, bytes) = read_path(engine, "restage.bin", 256 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0x99));
    assert_eq!(
        payload_names(&journal).len(),
        1,
        "evicted payload was not re-staged to the journal"
    );
    // The re-staged bytes count against the dirty ledger again.
    let mut dirty_free = u64::MAX;
    assert_eq!(
        unsafe { mirage_ffi::mirage_engine_dirty_free(engine, &mut dirty_free) },
        MirageStatus::Ok
    );
    assert_eq!(BUDGET - dirty_free, 256 * 1024);
    // Second read still exact (served from the re-staged file).
    let (status, bytes) = read_path(engine, "restage.bin", 256 * 1024);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0x99));
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn restage_stays_transient_when_budget_cannot_admit() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_with_budget(&index, state.path(), objects.path(), 64 * 1024);
    let journal = state.path().join("journal");

    // A = 40 KiB published; B = 40 KiB unpublished pushes the 64 KiB budget.
    // Keep the same pressure boundary without making debug encryption speed
    // on a busy developer machine part of this correctness gate.
    // into admission-eviction so A is remote-only with the budget nearly full.
    create_write(engine, "a.bin", &vec![0xAB; 40 * 1024]);
    wait_for(engine, |s| s.published_payloads >= 1);
    create_write(engine, "b.bin", &vec![0xCD; 40 * 1024]);
    assert!(stats(engine).evicted_payloads >= 1);
    assert_eq!(payload_names(&journal).len(), 1);

    // Reading A must not re-stage: the ledger cannot admit another 40 KiB.
    let (status, bytes) = read_path(engine, "a.bin", 4096);
    assert_eq!(status, MirageStatus::Ok);
    assert!(bytes.iter().all(|b| *b == 0xAB));
    assert_eq!(
        payload_names(&journal).len(),
        1,
        "budget-full restage must stay transient"
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn dead_extents_leave_the_unpublished_scan() {
    // Ghost pending payloads: a file whose journal payload was released
    // (extent dead, file deleted) can never publish — it must not keep the
    // publisher's candidate list non-empty.
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    let db = control_db(state.path());
    // The live publisher can claim a payload between write and scan, so keep
    // writing ghosts until one is observed pending, then kill its extent.
    let mut payload_id = None;
    for attempt in 0..40_u32 {
        create_write(
            engine,
            &format!("ghost-{attempt}.bin"),
            &vec![0x44; 128 * 1024],
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while payload_id.is_none() && std::time::Instant::now() < deadline {
            if let Some(pending) = db
                .unpublished_payloads(volume_id())
                .expect("pending")
                .first()
            {
                payload_id = Some(pending.payload_id);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        if payload_id.is_some() {
            break;
        }
    }
    let payload_id = payload_id.expect("a pending payload was never observed");
    db.writer()
        .physical_mark_extent_dead(payload_id, 1)
        .expect("mark dead");
    assert!(
        db.unpublished_payloads(volume_id())
            .expect("pending after dead")
            .iter()
            .all(|pending| pending.payload_id != payload_id),
        "dead-extent payload must leave the unpublished scan"
    );
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
#[cfg(windows)]
fn failed_file_deletion_keeps_payload_charged_and_retryable() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::atomic::Ordering;
    let (_source, objects, index) = build_index();
    let state = provisioned_state();
    let engine = managed_local(&index, state.path(), objects.path());
    create_write(engine, "locked.bin", &[0x71; 4096]);
    wait_for(engine, |s| s.published_payloads == 1);
    let journal = state.path().join("journal");
    let payload = std::fs::read_dir(&journal)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| entry.path().extension().is_some_and(|ext| ext == "payload"))
        .unwrap()
        .path();
    // Deny FILE_SHARE_DELETE while allowing readers and writers.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&payload)
        .unwrap();
    let before = unsafe { &*engine }
        .dirty
        .as_ref()
        .unwrap()
        .used
        .load(Ordering::Acquire);
    let (status, freed) = evict(engine, u64::MAX);
    assert_eq!(status, MirageStatus::IoError);
    assert_eq!(freed, 0);
    assert_eq!(stats(engine).evicted_payloads, 0);
    assert_eq!(
        unsafe { &*engine }
            .dirty
            .as_ref()
            .unwrap()
            .used
            .load(Ordering::Acquire),
        before
    );
    assert!(payload.exists());
    drop(held);
    let (status, freed) = evict(engine, u64::MAX);
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(freed, 4096);
    assert_eq!(stats(engine).evicted_payloads, 1);
    let (status, bytes) = read_path(engine, "locked.bin", 4096);
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, vec![0x71; 4096]);
    assert_eq!(unsafe { mirage_engine_destroy(engine) }, MirageStatus::Ok);
}

#[test]
fn deep_descendants_keep_their_ancestor_pin() {
    let dir = tempfile::tempdir().unwrap();
    let db = mirage_db::Database::open(&dir.path().join("control.db")).unwrap();
    let volume = volume_id();
    let root = db.namespace_create_volume(volume, 1).unwrap();
    let mut parent = root;
    for _ in 0..140 {
        parent = db
            .namespace_create(
                volume,
                parent,
                "d",
                mirage_db::NamespaceNodeKind::Directory,
                1,
            )
            .unwrap()
            .inode;
    }
    let pins = std::collections::HashSet::from([root]);
    assert!(mirage_ffi::publisher::pin_held(&db, volume, parent, &pins));
}
