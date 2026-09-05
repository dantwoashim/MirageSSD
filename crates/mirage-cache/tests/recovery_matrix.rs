use std::sync::Arc;

use mirage_cache::{
    ArenaShard, CacheLayout, ReconcileAction, ResidentIndex, SlotMetadata, SlotState, insert_page,
    reconcile,
};
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
    let bytes = vec![3; 60 * 1024];
    let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert");
    (directory, db, shard, hash)
}

#[test]
fn mismatched_resident_is_quarantined_and_repair_is_idempotent() {
    let (_directory, db, shard, hash) = setup();
    let record = db
        .load_resident_cache_slots()
        .expect("slots")
        .into_iter()
        .next()
        .expect("resident");
    shard
        .write_metadata(
            record.slot_index,
            SlotMetadata {
                generation: record.generation,
                state: SlotState::Free,
                page_hash: PageHash::from_bytes([0; 32]),
                logical_length: 0,
            },
        )
        .expect("corrupt metadata");
    shard.flush_metadata().expect("flush");

    let dry = reconcile(&db, &shard, true).expect("dry run");
    assert!(
        dry.actions
            .contains(&(record.slot_index, ReconcileAction::QuarantineResident))
    );
    assert!(ResidentIndex::rebuild(&db, Arc::clone(&shard)).is_ok());
    let applied = reconcile(&db, &shard, false).expect("repair");
    assert_eq!(applied.repaired, 1);
    assert!(
        db.load_resident_cache_slots()
            .expect("lookup")
            .into_iter()
            .all(|slot| slot.page_hash != Some(hash))
    );
    let clean = reconcile(&db, &shard, true).expect("second dry run");
    assert!(clean.actions.is_empty());
    assert!(clean.blocked.is_empty());
}
