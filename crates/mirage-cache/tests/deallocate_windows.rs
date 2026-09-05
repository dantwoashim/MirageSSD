use mirage_cache::{ArenaShard, CacheLayout, CacheUsage, ResidentIndex, insert_page};
use mirage_db::{CacheShardSpec, Database};
use mirage_types::{ByteCount, PageHash};
use std::sync::Arc;

#[test]
fn fill_evict_reclaims_sparse_allocation_and_accounting_stays_bounded() {
    let directory = tempfile::tempdir().expect("directory");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(1024 * 1024),
        slot_count: 8,
        db_journal_allowance: ByteCount::from_u64(8 * 1024 * 1024),
        filesystem_reserve: ByteCount::from_u64(16 * 1024 * 1024),
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
    let before = shard.diagnostics().expect("before").allocated_bytes;
    let bytes = (0_u8..=254).cycle().take(1024 * 1024).collect::<Vec<_>>();
    let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert");
    let filled = shard.diagnostics().expect("filled").allocated_bytes;
    assert!(filled >= before);
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index");
    assert!(index.evict(&db, hash).expect("evict"));
    assert!(!index.evict(&db, hash).expect("idempotent"));
    let after = shard.diagnostics().expect("after").allocated_bytes;
    #[cfg(windows)]
    assert!(
        after < filled,
        "sparse deallocation did not reclaim allocation: {filled} -> {after}"
    );
    let usage = CacheUsage::measure(&shard, layout, 0, 0, 0, 64 * 1024 * 1024).expect("usage");
    assert!(usage.headroom_bytes <= usage.hard_budget_bytes);
    assert_eq!(usage.ntfs_allocated_bytes, after);
}

#[test]
fn reports_physical_allocation_for_an_individual_slot() {
    let directory = tempfile::tempdir().expect("directory");
    let layout = CacheLayout {
        page_size: ByteCount::from_u64(1024 * 1024),
        slot_count: 2,
        db_journal_allowance: ByteCount::from_u64(8 * 1024 * 1024),
        filesystem_reserve: ByteCount::from_u64(16 * 1024 * 1024),
    };
    let shard = ArenaShard::create(&directory.path().join("arena.bin"), layout).expect("shard");
    assert_eq!(shard.reclaimable_slot_bytes(0).expect("empty slot"), 0);

    shard
        .write_slot(0, &vec![0x5a; 1024 * 1024])
        .expect("write");
    shard.flush().expect("flush");
    let allocated = shard.reclaimable_slot_bytes(0).expect("allocated slot");
    assert!(allocated > 0);
    assert!(allocated <= 1024 * 1024);

    shard.deallocate_slot(0).expect("deallocate");
    shard.flush().expect("flush deallocation");
    assert_eq!(
        shard.reclaimable_slot_bytes(0).expect("deallocated slot"),
        0
    );
}
