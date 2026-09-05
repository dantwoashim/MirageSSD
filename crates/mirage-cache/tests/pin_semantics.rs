use std::sync::Arc;

use mirage_cache::{ArenaShard, CacheLayout, PinReason, ResidentIndex, insert_page};
use mirage_db::{CacheShardSpec, Database};
use mirage_types::{ByteCount, PageHash};

fn setup() -> (tempfile::TempDir, Database, Arc<ArenaShard>, PageHash) {
    let directory = tempfile::tempdir().expect("directory");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(64 * 1024),
        slot_count: 4,
        db_journal_allowance: ByteCount::from_u64(1024 * 1024),
        filesystem_reserve: ByteCount::from_u64(1024 * 1024),
    };
    let shard =
        Arc::new(ArenaShard::create(&directory.path().join("arena.bin"), layout).expect("shard"));
    let db = Database::open(&directory.path().join("control.db")).expect("database");
    db.register_cache_shard(CacheShardSpec {
        shard_id: 0,
        relative_path: "arena.bin".into(),
        page_size: layout.page_size,
        slot_count: layout.slot_count,
    })
    .expect("register");
    let bytes = vec![7; 60 * 1024];
    let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert");
    (directory, db, shard, hash)
}

#[test]
fn overlapping_pins_are_durable_idempotent_and_block_eviction() {
    let (_directory, db, shard, hash) = setup();
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index");
    index
        .pins()
        .pin_batch(&db, PinReason::Mandatory, &[hash, hash])
        .expect("mandatory pin");
    index
        .pins()
        .pin_batch(&db, PinReason::Recovery, &[hash])
        .expect("recovery pin");
    assert!(index.evict(&db, hash).is_err());
    index
        .pins()
        .release(&db, PinReason::Mandatory)
        .expect("release mandatory");
    assert!(index.evict(&db, hash).is_err());

    let rebuilt = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("rebuild");
    assert!(rebuilt.pins().is_pinned(hash).expect("pin state"));
    rebuilt
        .pins()
        .release(&db, PinReason::Recovery)
        .expect("release recovery");
    assert!(rebuilt.evict(&db, hash).expect("evict"));
}

#[test]
fn batch_pin_is_all_or_nothing() {
    let (_directory, db, shard, hash) = setup();
    let index = ResidentIndex::rebuild(&db, shard).expect("index");
    let missing = PageHash::from_bytes([99; 32]);
    assert!(
        index
            .pins()
            .pin_batch(&db, PinReason::Mandatory, &[hash, missing])
            .is_err()
    );
    assert!(db.load_cache_pins().expect("pins").is_empty());
    assert!(!index.pins().is_pinned(hash).expect("pin state"));
}
