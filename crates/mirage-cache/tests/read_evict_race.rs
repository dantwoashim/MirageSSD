use std::sync::{Arc, Barrier};

use mirage_cache::{ArenaShard, CacheLayout, InsertOutcome, ResidentIndex, insert_page};
use mirage_db::{CacheShardSpec, Database};
use mirage_types::{ByteCount, PageHash};

#[test]
fn thousands_of_read_leases_never_overlap_reuse_or_eviction() {
    let directory = tempfile::tempdir().expect("directory");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(64 * 1024),
        slot_count: 4,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let shard =
        Arc::new(ArenaShard::create(&directory.path().join("arena.bin"), layout).expect("shard"));
    let db = Database::open(&directory.path().join("control.db")).expect("db");
    db.register_cache_shard(CacheShardSpec {
        shard_id: 0,
        relative_path: "arena.bin".into(),
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    })
    .expect("register");
    let bytes = vec![0x5a; 64 * 1024];
    let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    assert!(matches!(
        insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert"),
        InsertOutcome::Inserted(_)
    ));
    let index = Arc::new(ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index"));
    let barrier = Arc::new(Barrier::new(17));
    let mut workers = Vec::new();
    for _ in 0..16 {
        let index = Arc::clone(&index);
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..1000 {
                if let Some(guard) = index.acquire(hash).expect("lookup") {
                    let mut sample = [0_u8; 32];
                    guard.read_exact(17, &mut sample).expect("read");
                    assert_eq!(sample, [0x5a; 32]);
                }
            }
        }));
    }
    barrier.wait();
    while !workers.iter().all(std::thread::JoinHandle::is_finished) {
        let _ = index.evict(&db, hash);
        std::thread::yield_now();
    }
    for worker in workers {
        worker.join().expect("reader");
    }
    if index.acquire(hash).expect("lookup").is_some() {
        assert!(index.evict(&db, hash).expect("final eviction"));
    }
    assert!(index.acquire(hash).expect("lookup").is_none());
}

#[test]
fn tail_bounds_and_failed_deallocation_keep_page_unavailable() {
    let directory = tempfile::tempdir().expect("directory");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(64 * 1024),
        slot_count: 2,
        db_journal_allowance: ByteCount::ZERO,
        filesystem_reserve: ByteCount::ZERO,
    };
    let shard =
        Arc::new(ArenaShard::create(&directory.path().join("arena.bin"), layout).expect("shard"));
    let db = Database::open(&directory.path().join("control.db")).expect("db");
    db.register_cache_shard(CacheShardSpec {
        shard_id: 0,
        relative_path: "arena.bin".into(),
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    })
    .expect("register");
    let bytes = vec![0x11; 123];
    let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert");
    let index = ResidentIndex::rebuild(&db, shard).expect("index");
    let guard = index.acquire(hash).expect("lookup").expect("resident");
    let mut two = [0_u8; 2];
    assert!(guard.read_exact(122, &mut two).is_err());
    drop(guard);
    assert!(
        index
            .evict_with_deallocator(&db, hash, |_| Err(mirage_types::MirageError::cancelled(
                "injected deallocation failure"
            )))
            .is_err()
    );
    assert!(index.acquire(hash).expect("lookup").is_none());
    assert!(
        db.load_resident_cache_slots()
            .expect("residents")
            .is_empty()
    );
}
