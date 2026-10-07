#![allow(unsafe_code)]

//! Remote packing: small journal payloads publish inside ONE provider object
//! (`member_offset` rows), cutting per-object Drive round trips. Big payloads
//! still publish alone; a corrupt member is refused without blocking peers.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MiragePublicationStats, MirageStatus,
    mirage_engine_create_managed_local_provider, mirage_engine_destroy, mirage_engine_mark_mounted,
    mirage_engine_publication_stats, mirage_engine_set_drive_token, mirage_file_close,
    mirage_flush, mirage_lookup, mirage_namespace_create, mirage_read, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const BUDGET: u64 = 64 * 1024 * 1024;
const VOLUME: RepositoryId = RepositoryId::from_bytes([9; 16]);

fn build_index() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let source = tempfile::tempdir().expect("source");
    std::fs::write(source.path().join("base.dat"), vec![0x5Au8; 100 * 1024]).expect("seed");
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

fn managed_local(
    index: &Path,
    state_root: &Path,
    provider_root: &Path,
    budget: u64,
    token: bool,
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
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    if token {
        assert_eq!(
            unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
            MirageStatus::Ok
        );
    }
    engine
}

fn create_write(engine: *mut MirageEngineHandle, path: &str, bytes: &[u8]) {
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
    assert_eq!(
        unsafe { mirage_write(file, 0, bytes.as_ptr(), bytes.len(), &mut transferred) },
        MirageStatus::Ok
    );
    assert_eq!(transferred, bytes.len());
    assert_eq!(unsafe { mirage_flush(file) }, MirageStatus::Ok);
    unsafe { mirage_file_close(file) };
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
    panic!(
        "publisher stats never satisfied the predicate: {:?}",
        stats(engine)
    );
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
    unsafe { mirage_file_close(file) };
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

fn object_names(provider_root: &Path) -> Vec<String> {
    let payloads = provider_root.join("payloads");
    if !payloads.is_dir() {
        return Vec::new();
    }
    let mut names: Vec<String> = std::fs::read_dir(payloads)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn control_db(state_root: &Path) -> mirage_db::Database {
    mirage_db::Database::open(&state_root.join("control.db")).expect("control.db")
}

/// The dirty-extent payload id committed for `name`.
fn payload_id_of(db: &mirage_db::Database, name: &str) -> [u8; 16] {
    let inode = db
        .namespace_resolve_components(VOLUME, &[name.to_string()])
        .expect("resolve")
        .expect("inode");
    let (version, _) = db.extent_head(VOLUME, inode).expect("head").expect("head");
    db.extents_at(VOLUME, inode, version)
        .expect("extents")
        .iter()
        .find(|extent| extent.payload_id.is_some())
        .and_then(|extent| extent.payload_id)
        .expect("dirty extent")
}

fn hex16(bytes: [u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// (a) Small payloads pack into ONE provider object: N rows share the
/// provider_object_id and carry distinct member offsets.
#[test]
fn small_payloads_pack_into_one_object() {
    const FILES: usize = 8;
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    // Token withheld until every payload is pending, so one pass packs all.
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET, false);
    for i in 0..FILES {
        let content: Vec<u8> = (0..4096u32)
            .map(|j| ((i * 97 + j as usize) % 251) as u8)
            .collect();
        create_write(engine, &format!("pack-{i}.bin"), &content);
    }
    assert_eq!(
        unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
        MirageStatus::Ok
    );
    wait_for(engine, |s| s.published_payloads == FILES as u64);
    let names = object_names(objects.path());
    assert_eq!(names.len(), 1, "all members share one remote object");

    let db = control_db(state.path());
    let mut offsets = Vec::new();
    let mut object_ids = std::collections::BTreeSet::new();
    for i in 0..FILES {
        let payload_id = payload_id_of(&db, &format!("pack-{i}.bin"));
        let record = db
            .payload_publication(VOLUME, &payload_id)
            .expect("publication row")
            .expect("published");
        object_ids.insert(record.provider_object_id.clone());
        offsets.push(record.member_offset);
    }
    assert_eq!(object_ids.len(), 1, "all rows name the same object");
    offsets.sort_unstable();
    offsets.dedup();
    assert_eq!(offsets.len(), FILES, "member offsets are distinct");
    assert_eq!(offsets[0], 0, "first member at offset 0");
    drop(db);
    unsafe { mirage_engine_destroy(engine) };
}

/// (b) Every member reads back byte-exact after local eviction — frame
/// ranges resolve through member_offset (the restage path).
#[test]
fn packed_members_read_back_after_eviction() {
    const FILES: usize = 4;
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET, false);
    let mut contents = Vec::new();
    for i in 0..FILES {
        let content: Vec<u8> = (0..40_000u32)
            .map(|j| ((i * 131 + j as usize) % 251) as u8)
            .collect();
        create_write(engine, &format!("member-{i}.bin"), &content);
        contents.push(content);
    }
    assert_eq!(
        unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
        MirageStatus::Ok
    );
    wait_for(engine, |s| s.published_payloads == FILES as u64);
    assert_eq!(object_names(objects.path()).len(), 1);
    // Evict every local payload — each read must restage through the pack.
    let journal = state.path().join("journal");
    for name in payload_names(&journal) {
        std::fs::remove_file(journal.join(name)).expect("evict payload");
    }
    for (i, content) in contents.iter().enumerate() {
        let (status, bytes) = read_path(engine, &format!("member-{i}.bin"), content.len());
        assert_eq!(status, MirageStatus::Ok);
        assert_eq!(&bytes, content, "member {i} byte-exact through the pack");
    }
    assert_eq!(stats(engine).integrity_refusals, 0);
    unsafe { mirage_engine_destroy(engine) };
}

/// (c) A payload over the member cap publishes as its own object — the
/// small file still packs separately.
#[test]
fn oversized_payload_publishes_alone() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET, false);
    let big: Vec<u8> = (0..(5 * 1024 * 1024u64)).map(|i| (i % 251) as u8).collect();
    let small: Vec<u8> = (0..4096u32).map(|i| (i % 197) as u8).collect();
    create_write(engine, "big.bin", &big);
    create_write(engine, "small.bin", &small);
    assert_eq!(
        unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
        MirageStatus::Ok
    );
    wait_for(engine, |s| s.published_payloads == 2);
    assert_eq!(
        object_names(objects.path()).len(),
        2,
        "big alone + small pack"
    );

    let db = control_db(state.path());
    let big_record = db
        .payload_publication(VOLUME, &payload_id_of(&db, "big.bin"))
        .unwrap()
        .unwrap();
    assert_eq!(big_record.member_offset, 0);
    assert!(
        big_record.object_length > 5 * 1024 * 1024,
        "big object carries only its own frames"
    );
    drop(db);
    unsafe { mirage_engine_destroy(engine) };
}

/// (d) A checksum-mismatched member is refused while the pack's other
/// members publish on the same pass.
#[test]
fn corrupt_member_refused_others_publish() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    // No token yet: the publisher idles until the credential lands, so both
    // payloads are still pending when we corrupt one.
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET, false);
    create_write(engine, "good.bin", &[7u8; 4000]);
    create_write(engine, "bad.bin", &[3u8; 4000]);

    let db = control_db(state.path());
    let bad_payload = payload_id_of(&db, "bad.bin");
    let path = state
        .path()
        .join("journal")
        .join(format!("{}.payload", hex16(bad_payload)));
    let mut bytes = std::fs::read(&path).expect("payload file");
    bytes[0] ^= 0xFF;
    std::fs::write(&path, bytes).expect("flip a byte");
    drop(db);

    assert_eq!(
        unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
        MirageStatus::Ok
    );
    wait_for(engine, |s| {
        s.published_payloads == 1 && s.integrity_refusals >= 1
    });
    assert_eq!(
        object_names(objects.path()).len(),
        1,
        "the good member's object still lands"
    );
    let (status, bytes) = read_path(engine, "good.bin", 4000);
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, vec![7u8; 4000]);
    assert_eq!(
        stats(engine).pending_payloads,
        1,
        "bad member stays pending"
    );
    unsafe { mirage_engine_destroy(engine) };
}
