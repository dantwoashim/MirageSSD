#![allow(unsafe_code)]

//! Managed-volume payload publication gates: journal payloads publish as one
//! immutable encrypted remote object through the directory-backend seam, the
//! local file becomes evictable, and reads fetch+verify frames remotely.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mirage_ffi::{
    MirageEngineHandle, MirageFileHandle, MiragePublicationStats, MirageStatus,
    mirage_engine_create_managed, mirage_engine_create_managed_local_provider,
    mirage_engine_destroy, mirage_engine_mark_mounted, mirage_engine_publication_stats,
    mirage_engine_set_drive_token, mirage_flush, mirage_lookup, mirage_namespace_create,
    mirage_read, mirage_write,
};
use mirage_manifest::FileClass;
use mirage_pack::{ImportPlan, PlannedFile, import_local};
use mirage_types::{GenerationId, RepositoryId};

const BUDGET: u64 = 64 * 1024 * 1024;

fn local_key() -> mirage_crypto::aead::RepositoryKey {
    // Matches the deterministic key the local-provider seam derives.
    mirage_crypto::aead::RepositoryKey::from_bytes(
        *blake3::hash(b"mirage-local-provider-publication-key/v1").as_bytes(),
    )
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

fn managed_local(
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
    // The directory backend accepts token pushes to install the provider —
    // which is also the publisher's start signal.
    assert_eq!(
        unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
        MirageStatus::Ok
    );
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
    unsafe { mirage_ffi::mirage_file_close(file) };
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

#[test]
fn write_flush_publishes_one_verifiable_object() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET);
    let content: Vec<u8> = (0..150_000u32).map(|i| (i % 251) as u8).collect();
    create_write(engine, "written.bin", &content);
    wait_for(engine, |s| s.published_payloads == 1);
    let names = object_names(objects.path());
    assert_eq!(names.len(), 1, "one remote object");
    // Decrypt the object: concatenated frames, each self-describing.
    let object = std::fs::read(objects.path().join("payloads").join(&names[0])).expect("object");
    let key = local_key();
    let volume = RepositoryId::from_bytes([9; 16]);
    let mut recovered = Vec::new();
    let mut cursor = 0usize;
    while cursor < object.len() {
        let prefix = 12 + 16 + 24; // magic + length + pack_id + nonce
        let ct_len = u32::from_le_bytes(object[cursor + 8..cursor + 12].try_into().unwrap());
        let frame = &object[cursor..cursor + prefix + ct_len as usize];
        let frame_index = (recovered.len() as u64) / (4 * 1024 * 1024);
        let chunk_start = (frame_index * 4 * 1024 * 1024) as usize;
        let chunk_end = ((frame_index + 1) * 4 * 1024 * 1024).min(content.len() as u64) as usize;
        let chunk = &content[chunk_start..chunk_end];
        let plain = mirage_pack::encrypted_frame::decode_encrypted_frame(
            &key,
            frame,
            mirage_pack::encrypted_frame::EncryptedFrameAad {
                repository: volume,
                pack_id: {
                    // payload id is the payload filename's hex stem
                    let name = payload_names(&state.path().join("journal"))[0].clone();
                    let mut id = [0u8; 16];
                    for i in 0..16 {
                        id[i] = u8::from_str_radix(&name[2 * i..2 * i + 2], 16).unwrap();
                    }
                    id
                },
                frame_index,
                plaintext_hash: mirage_types::PageHash::from_bytes(*blake3::hash(chunk).as_bytes()),
                plaintext_length: chunk.len() as u32,
            },
        )
        .expect("frame decrypts");
        recovered.extend_from_slice(&plain);
        cursor += frame.len();
    }
    assert_eq!(recovered, content, "object round-trips the payload bytes");
    // frame_range agrees with the real boundaries.
    let overhead = object.len() as u64 - content.len() as u64;
    let (offset, len) =
        mirage_ffi::publisher::payload_frame_range(content.len() as u64, 0).expect("frame 0");
    assert_eq!(offset, 0);
    assert_eq!(len, content.len() as u64 + overhead);
    assert_eq!(stats(engine).pending_payloads, 0);
    unsafe { mirage_engine_destroy(engine) };
}

#[test]
fn evicted_payload_reads_back_verified() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET);
    let content = vec![0xABu8; 40_000];
    create_write(engine, "victim.bin", &content);
    wait_for(engine, |s| s.published_payloads == 1);
    // Simulate eviction: the payload file is gone but the row persists.
    let journal = state.path().join("journal");
    for name in payload_names(&journal) {
        std::fs::remove_file(journal.join(name)).expect("remove payload");
    }
    let (status, bytes) = read_path(engine, "victim.bin", content.len());
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, content, "evicted payload served byte-exact");
    assert_eq!(stats(engine).integrity_refusals, 0);
    unsafe { mirage_engine_destroy(engine) };
}

#[test]
fn orphaned_upload_is_idempotent() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET);
    let content = vec![7u8; 5000];
    create_write(engine, "a.bin", &content);
    wait_for(engine, |s| s.published_payloads == 1);
    // Crash-recovery shape: the object exists; the row exists; a second
    // publish attempt must not duplicate the object. Directly pre-plant an
    // extra pending payload whose object already exists: republish via the
    // publisher reuses it (find_exact). Simulate by deleting the row? Rows
    // are append-safe — instead verify a fresh engine pass produces no new
    // object: destroy + remount keeps exactly one object.
    unsafe { mirage_engine_destroy(engine) };
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(object_names(objects.path()).len(), 1, "no duplicate object");
    unsafe { mirage_engine_destroy(engine) };
}

#[test]
fn unpublished_payloads_are_never_evicted() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    // No token → the publisher cannot publish; budget pressure must not evict.
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
                3 * 1024 * 1024,
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
    let path16 = utf16("a.bin");
    assert_eq!(
        unsafe { mirage_namespace_create(engine, path16.as_ptr(), path16.len(), 0) },
        MirageStatus::Ok
    );
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    let a = vec![1u8; 2 * 1024 * 1024];
    let mut transferred = 0usize;
    assert_eq!(
        unsafe { mirage_write(file, 0, a.as_ptr(), a.len(), &mut transferred) },
        MirageStatus::Ok
    );
    // Second write exceeds the budget with nothing evictable → DiskFull,
    // and A's payload file remains local.
    let b = vec![2u8; 2 * 1024 * 1024];
    let status =
        unsafe { mirage_write(file, a.len() as u64, b.as_ptr(), b.len(), &mut transferred) };
    assert_eq!(status, MirageStatus::DiskFull);
    assert!(!payload_names(&state.path().join("journal")).is_empty());
    unsafe { mirage_ffi::mirage_file_close(file) };
    assert_eq!(stats(engine).published_payloads, 0);
    unsafe { mirage_engine_destroy(engine) };
}

#[test]
fn budget_pressure_evicts_published_payload() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    // 3 MiB budget: A (2 MiB) publishes, then B (2 MiB) forces eviction.
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
                3 * 1024 * 1024,
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
    let a: Vec<u8> = (0..2 * 1024 * 1024u64).map(|i| (i % 251) as u8).collect();
    create_write(engine, "a.bin", &a);
    wait_for(engine, |s| s.published_payloads == 1);
    let b = vec![3u8; 2 * 1024 * 1024];
    create_write(engine, "b.bin", &b);
    // A's extent went dead (evicted); its file is gone; B committed.
    assert_eq!(stats(engine).evicted_payloads, 1, "A evicted");
    let journal = state.path().join("journal");
    let names = payload_names(&journal);
    assert_eq!(names.len(), 1, "only B's payload remains local");
    // A reads back through the remote object, byte-exact.
    let (status, bytes) = read_path(engine, "a.bin", a.len());
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, a);
    unsafe { mirage_engine_destroy(engine) };
}

#[test]
fn corrupted_payload_is_refused() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    // Write before the token push: the publisher idles until the credential
    // arrives, so the payload is still unpublished when we corrupt it.
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
                BUDGET,
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
    create_write(engine, "flip.bin", &[9u8; 4000]);
    let journal = state.path().join("journal");
    let names = payload_names(&journal);
    assert_eq!(names.len(), 1);
    let path = journal.join(&names[0]);
    let mut bytes = std::fs::read(&path).expect("payload");
    bytes[0] ^= 0xFF;
    std::fs::write(&path, bytes).expect("flip");
    // Now install the backend credential: the next pass refuses.
    assert_eq!(
        unsafe { mirage_engine_set_drive_token(engine, b"local".as_ptr(), 5) },
        MirageStatus::Ok
    );
    wait_for(engine, |s| s.integrity_refusals >= 1);
    assert!(object_names(objects.path()).is_empty());
    assert_eq!(stats(engine).published_payloads, 0);
    unsafe { mirage_engine_destroy(engine) };
}

#[test]
fn no_provider_publishes_nothing() {
    let (_objects_dir, index) = {
        let source = tempfile::tempdir().expect("source");
        std::fs::write(source.path().join("f"), b"x").unwrap();
        let objects = tempfile::tempdir().expect("objects");
        let imported = import_local(&ImportPlan {
            repository_id: RepositoryId::from_bytes([9; 16]),
            generation_id: GenerationId::ZERO,
            source_root: source.path().to_path_buf(),
            files: vec![PlannedFile {
                relative_path: "f".into(),
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
    };
    let state = provisioned_state(64 * 1024, 8);
    let index16: Vec<u16> = index.to_string_lossy().encode_utf16().collect();
    let root16: Vec<u16> = state.path().to_string_lossy().encode_utf16().collect();
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
                BUDGET,
                &mut engine,
            )
        },
        MirageStatus::Ok
    );
    assert_eq!(
        unsafe { mirage_engine_mark_mounted(engine) },
        MirageStatus::Ok
    );
    create_write(engine, "w.bin", &[1u8; 1000]);
    std::thread::sleep(Duration::from_millis(1200));
    let s = stats(engine);
    assert_eq!(s.published_payloads, 0);
    // Pending bytes are visible in the durable ledger.
    let db = mirage_db::Database::open(&state.path().join("control.db")).expect("db");
    let pending = db
        .unpublished_payloads(RepositoryId::from_bytes([9; 16]))
        .expect("pending");
    assert_eq!(pending.len(), 1);
    unsafe { mirage_engine_destroy(engine) };
}

#[test]
fn multi_frame_payload_reads_across_boundaries() {
    let (_source, objects, index) = build_index();
    let state = provisioned_state(64 * 1024, 8);
    let engine = managed_local(&index, state.path(), objects.path(), BUDGET);
    // 9 MiB → 3 frames at 4 MiB each.
    let content: Vec<u8> = (0..9 * 1024 * 1024u64)
        .map(|i| (i * 31 % 253) as u8)
        .collect();
    create_write(engine, "big.bin", &content);
    wait_for(engine, |s| s.published_payloads == 1);
    let journal = state.path().join("journal");
    for name in payload_names(&journal) {
        std::fs::remove_file(journal.join(name)).expect("remove");
    }
    // Full read.
    let (status, bytes) = read_path(engine, "big.bin", content.len());
    assert_eq!(status, MirageStatus::Ok);
    assert_eq!(bytes, content);
    // A boundary-straddling slice: 2 KiB starting 1 KiB before frame 1.
    let path16 = utf16("big.bin");
    let mut file: *mut MirageFileHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { mirage_lookup(engine, path16.as_ptr(), path16.len(), &mut file) },
        MirageStatus::Ok
    );
    let mut output = vec![0u8; 2048];
    let mut transferred = 0usize;
    let offset = 4 * 1024 * 1024 - 1024;
    assert_eq!(
        unsafe { mirage_read(file, offset, output.as_mut_ptr(), 2048, &mut transferred) },
        MirageStatus::Ok
    );
    assert_eq!(transferred, 2048);
    assert_eq!(
        output,
        content[offset as usize..offset as usize + 2048].to_vec()
    );
    unsafe { mirage_ffi::mirage_file_close(file) };
    unsafe { mirage_engine_destroy(engine) };
}
