use std::sync::Arc;

use mirage_cache::{
    ArenaShard, CacheLayout, IntegrityClass, ResidentIndex, VerifyOutcome, insert_page,
    scrub_batch, verify_page,
};
use mirage_db::{CacheShardSpec, Database};
use mirage_types::{ByteCount, PageHash};

fn setup() -> (
    tempfile::TempDir,
    Database,
    Arc<ArenaShard>,
    ResidentIndex,
    PageHash,
) {
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
    let bytes = vec![5; 60 * 1024];
    let hash = PageHash::from_bytes(*blake3::hash(&bytes).as_bytes());
    let outcome = insert_page(&db, Arc::clone(&shard), hash, &bytes, &()).expect("insert");
    let slot = match outcome {
        mirage_cache::InsertOutcome::Inserted(record)
        | mirage_cache::InsertOutcome::Existing(record) => record.slot_index,
    };
    shard.write_slot(slot, &[8; 32]).expect("flip bytes");
    shard.flush().expect("flush corruption");
    let index = ResidentIndex::rebuild(&db, Arc::clone(&shard)).expect("index");
    (directory, db, shard, index, hash)
}

#[test]
fn clean_corruption_is_quarantined_and_never_returned_again() {
    let (_directory, db, _shard, index, hash) = setup();
    assert_eq!(
        verify_page(&index, &db, hash, IntegrityClass::Clean).expect("verify"),
        VerifyOutcome::Quarantined
    );
    assert!(index.acquire(hash).expect("lookup").is_none());
}

#[test]
fn protected_corruption_requires_recovery_without_deletion() {
    for class in [
        IntegrityClass::PinnedClean,
        IntegrityClass::Dirty,
        IntegrityClass::Staged,
    ] {
        let (_directory, db, _shard, index, hash) = setup();
        assert_eq!(
            verify_page(&index, &db, hash, class).expect("verify"),
            VerifyOutcome::RecoveryRequired
        );
        assert!(index.acquire(hash).expect("lookup").is_some());
    }
}

#[test]
fn scrub_is_bounded_and_pauses_for_gameplay() {
    let (_directory, db, _shard, index, _hash) = setup();
    assert!(scrub_batch(&index, &db, 10, true).expect("paused").paused);
    let report = scrub_batch(&index, &db, 1, false).expect("scrub");
    assert_eq!(report.selected, 1);
    assert_eq!(report.quarantined, 1);
}
